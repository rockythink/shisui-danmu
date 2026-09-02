mod client;
mod packet;
mod parser;

pub use client::{
    AccountClient, AccountStatus, BilibiliClient, BilibiliClientEvent, LoginChallenge, LoginPoll,
    RoomLiveStatus, RoomSnapshot,
};
pub use packet::{Packet, PacketError, encode_packet, parse_packets};
pub use parser::parse_command;

use crate::domain::{DanmuEvent, DanmuEventKind};
use chrono::Duration;
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;

pub const RECONNECT_DELAYS_SECONDS: &[u64] = &[1, 2, 4, 8, 15, 30];
pub const SEND_SEGMENT_LIMIT: usize = 20;
pub const ACCOUNT_MESSAGE_LIMIT: usize = 22;

pub fn segment_message(message: &str, limit: usize) -> Vec<String> {
    let trimmed = message.trim();
    if trimmed.is_empty() || limit == 0 {
        return Vec::new();
    }

    let graphemes = trimmed.graphemes(true).collect::<Vec<_>>();
    let mut segments = Vec::new();
    let mut start = 0;
    while start < graphemes.len() {
        let hard_end = (start + limit).min(graphemes.len());
        let end = if hard_end < graphemes.len() {
            let candidate = &graphemes[start..hard_end];
            candidate
                .iter()
                .rposition(|grapheme| is_segment_boundary(grapheme))
                .or_else(|| {
                    candidate
                        .iter()
                        .rposition(|grapheme| grapheme.trim().is_empty())
                })
                .map_or(hard_end, |index| start + index + 1)
        } else {
            hard_end
        };
        let segment = graphemes[start..end].concat();
        let segment = segment.trim();
        if !segment.is_empty() {
            segments.push(segment.to_string());
        }
        start = end;
        while graphemes
            .get(start)
            .is_some_and(|grapheme| grapheme.trim().is_empty())
        {
            start += 1;
        }
    }
    segments
}

fn is_segment_boundary(grapheme: &str) -> bool {
    matches!(
        grapheme,
        "。" | "！"
            | "？"
            | "；"
            | "，"
            | "、"
            | "："
            | "."
            | "!"
            | "?"
            | ";"
            | ","
            | ":"
            | "…"
            | "”"
            | "’"
            | "」"
            | "』"
            | "】"
            | "）"
            | ")"
    )
}

#[derive(Debug, Default)]
pub struct CrossOriginDeduplicator {
    recent: VecDeque<DanmuEvent>,
}

impl CrossOriginDeduplicator {
    pub fn should_emit(&mut self, event: &DanmuEvent) -> bool {
        let cutoff = event.timestamp - Duration::seconds(3);
        while self
            .recent
            .front()
            .is_some_and(|item| item.timestamp < cutoff)
        {
            self.recent.pop_front();
        }

        let duplicate = self
            .recent
            .iter()
            .any(|candidate| cross_origin_duplicate(candidate, event));
        self.recent.push_back(event.clone());
        !duplicate
    }
}

pub(crate) fn normalized_message(value: &str) -> String {
    value.split_whitespace().collect::<String>().to_lowercase()
}

pub(crate) fn cross_origin_duplicate(lhs: &DanmuEvent, rhs: &DanmuEvent) -> bool {
    lhs.origin != rhs.origin
        && lhs.kind == rhs.kind
        && normalized_message(&lhs.content) == normalized_message(&rhs.content)
        && authors_match(lhs, rhs)
        && (lhs.timestamp - rhs.timestamp).num_seconds().abs() <= 3
}

fn authors_match(lhs: &DanmuEvent, rhs: &DanmuEvent) -> bool {
    match (
        usable_author_id(lhs.author_id.as_deref()),
        usable_author_id(rhs.author_id.as_deref()),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => {
            let a = lhs
                .username
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            let b = rhs
                .username
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            !a.is_empty() && !b.is_empty() && (a == b || masked_name_matches(&a, &b))
        }
    }
}

pub(crate) fn usable_author_id(value: Option<&str>) -> Option<&str> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "0")
}

pub(crate) fn masked_name_matches(lhs: &str, rhs: &str) -> bool {
    fn matches(pattern: &str, value: &str) -> bool {
        if !pattern.contains('*') {
            return false;
        }
        let parts: Vec<_> = pattern.split('*').filter(|part| !part.is_empty()).collect();
        let mut offset = 0;
        for part in parts {
            let Some(index) = value[offset..].find(part) else {
                return false;
            };
            offset += index + part.len();
        }
        true
    }
    matches(lhs, rhs) || matches(rhs, lhs)
}

pub fn kind_label(kind: DanmuEventKind) -> &'static str {
    match kind {
        DanmuEventKind::Danmu => "弹幕",
        DanmuEventKind::Gift => "礼物",
        DanmuEventKind::GuardEvent => "大航海",
        DanmuEventKind::Superchat => "醒目留言",
        DanmuEventKind::Enter => "进场",
        DanmuEventKind::Like => "点赞",
        DanmuEventKind::Follow => "关注",
        DanmuEventKind::Share => "分享",
        DanmuEventKind::Pk => "PK",
        DanmuEventKind::Lottery => "抽奖",
        DanmuEventKind::Moderation => "管理",
        DanmuEventKind::RoomStatus => "直播间",
        DanmuEventKind::System => "系统",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_by_grapheme_cluster() {
        assert_eq!(segment_message("甲👨‍👩‍👧‍👦乙", 2), vec!["甲👨‍👩‍👧‍👦", "乙"]);
    }

    #[test]
    fn prefers_punctuation_when_segmenting_long_messages() {
        assert_eq!(
            segment_message("第一段，第二段，第三段", 6),
            vec!["第一段，", "第二段，", "第三段"]
        );
    }
    #[test]
    fn falls_back_to_whitespace_then_the_grapheme_limit() {
        let segments = segment_message("alpha, beta gamma delta", 8);

        assert_eq!(segments, vec!["alpha,", "beta", "gamma", "delta"]);
        assert!(
            segments
                .iter()
                .all(|segment| segment.graphemes(true).count() <= 8)
        );
    }
}
