use super::{clean_platform_markup, fit_display_width, usable_author_id};
use crate::domain::{DanmuEvent, DanmuEventKind, DanmuEventOrigin};
use chrono::{DateTime, Utc};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

const MIN_READ_TIME: Duration = Duration::from_secs(2);
const MAX_NAMES: usize = 3;

struct AudienceName {
    author_id: Option<String>,
    name: String,
    expires_at: Instant,
}

#[derive(Default)]
struct Batch {
    names: Vec<AudienceName>,
    more_until: Option<Instant>,
}

impl Batch {
    fn is_empty(&self) -> bool {
        self.names.is_empty() && self.more_until.is_none()
    }

    fn expire(&mut self, now: Instant) {
        self.names.retain(|name| now < name.expires_at);
        if self.more_until.is_some_and(|until| now >= until) {
            self.more_until = None;
        }
    }

    fn push(&mut self, event: &DanmuEvent, expires_at: Instant) {
        let author_id = usable_author_id(event.author_id.as_deref());
        if let Some(existing) = author_id.and_then(|id| {
            self.names
                .iter_mut()
                .find(|name| name.author_id.as_deref() == Some(id))
        }) {
            existing.expires_at = existing.expires_at.max(expires_at);
            return;
        }
        if self.names.len() == MAX_NAMES {
            self.more_until = Some(
                self.more_until
                    .map_or(expires_at, |until| until.max(expires_at)),
            );
            return;
        }
        let name = event
            .username
            .as_deref()
            .map(clean_platform_markup)
            .unwrap_or_default()
            .replace(['\r', '\n'], " ");
        self.names.push(AudienceName {
            author_id: author_id.map(str::to_owned),
            name: if name.trim().is_empty() {
                "观众".into()
            } else {
                name
            },
            expires_at,
        });
    }

    fn text(
        &self,
        kind: DanmuEventKind,
        width: usize,
        show_name: bool,
        truncate: bool,
    ) -> Option<String> {
        let action = if kind == DanmuEventKind::Enter {
            "进入直播间"
        } else {
            "为直播间点赞"
        };
        if show_name && !self.names.is_empty() {
            for count in (1..=self.names.len()).rev() {
                let mut names = String::new();
                for name in &self.names[..count] {
                    if !names.is_empty() {
                        names.push('、');
                    }
                    names.push_str(&name.name);
                }
                let more = if count < self.names.len() || self.more_until.is_some() {
                    "等"
                } else {
                    ""
                };
                let suffix = format!("{more} {action}");
                let text = format!("{names}{suffix}");
                if UnicodeWidthStr::width(text.as_str()) <= width {
                    return Some(text);
                }
                if count == 1 && truncate {
                    let name_width = width.saturating_sub(UnicodeWidthStr::width(suffix.as_str()));
                    if name_width > 0 {
                        return Some(format!("{}{suffix}", fit_display_width(&names, name_width)));
                    }
                    return Some(fit_display_width(&text, width));
                }
            }
            return None;
        }
        let text = format!("有观众{action}");
        if UnicodeWidthStr::width(text.as_str()) <= width {
            Some(text)
        } else {
            truncate.then(|| fit_display_width(&text, width))
        }
    }
}

struct ShownBatch {
    kind: DanmuEventKind,
    primary: Batch,
    likes: Batch,
    stable_until: Instant,
    expires_at: Instant,
    likes_until: Instant,
}

/// Only the visible batch and one bounded next-batch summary are retained.
#[derive(Default)]
pub(super) struct ActivityNotices {
    current: Option<ShownBatch>,
    next_enter: Batch,
    next_like: Batch,
}

impl ActivityNotices {
    pub(super) fn push(&mut self, event: &DanmuEvent, now: Instant, wall_now: DateTime<Utc>) {
        let Some(lifetime) = event.kind.activity_lifetime() else {
            return;
        };
        if event.origin != DanmuEventOrigin::Live {
            return;
        }
        // Delayed events may expire while pending; future platform clocks cannot extend their TTL.
        let remaining = (event.timestamp + lifetime - wall_now).min(lifetime);
        let Ok(remaining) = remaining.to_std() else {
            return;
        };
        if remaining.is_zero() {
            return;
        }
        self.next_enter.expire(now);
        self.next_like.expire(now);
        let pending = if event.kind == DanmuEventKind::Enter {
            &mut self.next_enter
        } else {
            &mut self.next_like
        };
        pending.push(event, now + remaining);
        self.advance(now);
    }

    pub(super) fn advance(&mut self, now: Instant) {
        self.next_enter.expire(now);
        self.next_like.expire(now);
        if self
            .current
            .as_ref()
            .is_some_and(|current| now < current.stable_until)
        {
            return;
        }
        if self
            .current
            .as_ref()
            .is_some_and(|current| now >= current.expires_at)
        {
            self.current = None;
        }
        if !self.next_enter.is_empty() {
            self.current = Some(ShownBatch {
                kind: DanmuEventKind::Enter,
                primary: std::mem::take(&mut self.next_enter),
                likes: std::mem::take(&mut self.next_like),
                stable_until: now + MIN_READ_TIME,
                expires_at: now + Duration::from_secs(5),
                likes_until: now + Duration::from_secs(3),
            });
        } else if !self.next_like.is_empty() {
            if let Some(current) = self
                .current
                .as_mut()
                .filter(|current| current.kind == DanmuEventKind::Enter)
            {
                // Do not displace a welcome with likes. Add a readable suffix when time permits.
                if (current.likes.is_empty() || now >= current.likes_until)
                    && now + MIN_READ_TIME <= current.expires_at
                {
                    current.likes = std::mem::take(&mut self.next_like);
                    current.likes_until = (now + Duration::from_secs(3)).min(current.expires_at);
                    current.stable_until = now + MIN_READ_TIME;
                }
            } else {
                self.current = Some(ShownBatch {
                    kind: DanmuEventKind::Like,
                    primary: std::mem::take(&mut self.next_like),
                    likes: Batch::default(),
                    stable_until: now + MIN_READ_TIME,
                    expires_at: now + Duration::from_secs(3),
                    likes_until: now,
                });
            }
        }
    }

    pub(super) fn render(
        &self,
        width: usize,
        show_name: bool,
        now: Instant,
    ) -> Option<(DanmuEventKind, String)> {
        let current = self
            .current
            .as_ref()
            .filter(|current| now < current.expires_at)?;
        let mut text = current.primary.text(current.kind, width, show_name, true)?;
        if !current.likes.is_empty() && now < current.likes_until {
            let remaining = width.saturating_sub(UnicodeWidthStr::width(text.as_str()) + 3);
            if let Some(likes) =
                current
                    .likes
                    .text(DanmuEventKind::Like, remaining, show_name, false)
            {
                text.push_str(" · ");
                text.push_str(&likes);
            }
        }
        Some((current.kind, text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: DanmuEventKind, name: &str, id: Option<&str>, at: DateTime<Utc>) -> DanmuEvent {
        let mut event = DanmuEvent::new(kind, format!("{name} 的原始互动"));
        event.timestamp = at;
        event.username = Some(name.into());
        event.author_id = id.map(str::to_owned);
        event
    }

    fn text(
        notices: &mut ActivityNotices,
        now: Instant,
        width: usize,
        names: bool,
    ) -> Option<String> {
        notices.advance(now);
        notices.render(width, names, now).map(|(_, text)| text)
    }

    #[test]
    fn keeps_names_readable_then_replaces_them_with_the_next_batch() {
        let now = Instant::now();
        let wall = Utc::now();
        let mut notices = ActivityNotices::default();
        notices.push(
            &event(DanmuEventKind::Enter, "张三", Some("1"), wall),
            now,
            wall,
        );
        let first = text(&mut notices, now, 80, true).unwrap();
        for (millis, name, id) in [(400, "李四", "2"), (800, "王五", "3")] {
            let at = wall + chrono::Duration::milliseconds(millis);
            notices.push(
                &event(DanmuEventKind::Enter, name, Some(id), at),
                now + Duration::from_millis(millis as u64),
                at,
            );
        }
        assert_eq!(
            text(&mut notices, now + Duration::from_millis(1999), 80, true).unwrap(),
            first
        );
        let next = text(&mut notices, now + Duration::from_secs(2), 80, true).unwrap();
        assert!(next.contains("李四、王五"));
        assert!(!next.contains("张三"));
        assert!(text(&mut notices, now + Duration::from_millis(6999), 80, true).is_some());
        assert!(text(&mut notices, now + Duration::from_secs(7), 80, true).is_none());
        assert!(text(&mut notices, now + Duration::from_secs(20), 80, true).is_none());
    }

    #[test]
    fn deduplicates_only_reliable_ids_and_preserves_same_named_viewers() {
        let now = Instant::now();
        let wall = Utc::now();
        for ids in [
            [Some("a"), Some("a")],
            [Some("a"), Some("b")],
            [Some("0"), Some("0")],
            [None, None],
        ] {
            let mut notices = ActivityNotices::default();
            notices.push(
                &event(DanmuEventKind::Like, "上一批", Some("seed"), wall),
                now,
                wall,
            );
            for id in ids {
                notices.push(
                    &event(DanmuEventKind::Like, "同名观众", id, wall),
                    now,
                    wall,
                );
            }
            let rendered = text(&mut notices, now + MIN_READ_TIME, 80, true).unwrap();
            let expected = if ids == [Some("a"), Some("a")] { 1 } else { 2 };
            assert_eq!(rendered.matches("同名观众").count(), expected, "{rendered}");
            assert!(!rendered.contains("等"));
        }
    }

    #[test]
    fn burst_prioritizes_welcomes_with_optional_likes_and_never_builds_a_replay_queue() {
        let now = Instant::now();
        let wall = Utc::now();
        let mut notices = ActivityNotices::default();
        notices.push(
            &event(DanmuEventKind::Like, "首位", Some("seed"), wall),
            now,
            wall,
        );
        for index in 0..1000 {
            let kind = if index % 2 == 0 {
                DanmuEventKind::Enter
            } else {
                DanmuEventKind::Like
            };
            notices.push(
                &event(
                    kind,
                    &format!("观众{index}"),
                    Some(&index.to_string()),
                    wall,
                ),
                now,
                wall,
            );
        }
        let first = text(&mut notices, now + Duration::from_millis(1999), 120, true).unwrap();
        assert!(first.contains("首位"));
        let wide = text(&mut notices, now + MIN_READ_TIME, 120, true).unwrap();
        assert!(wide.contains("观众0、观众2、观众4等"), "{wide}");
        assert!(wide.contains("观众1、观众3、观众5等"), "{wide}");
        assert!(!wide.contains("首位"));
        let narrow = text(&mut notices, now + MIN_READ_TIME, 28, true).unwrap();
        assert!(narrow.contains("进入直播间"));
        assert!(!narrow.contains("点赞"));
        let expired_likes = text(&mut notices, now + Duration::from_secs(5), 120, true).unwrap();
        assert!(expired_likes.contains("进入直播间"));
        assert!(!expired_likes.contains("点赞"));
        assert!(text(&mut notices, now + Duration::from_secs(7), 120, true).is_none());
    }

    #[test]
    fn likes_do_not_displace_a_welcome_and_stale_pending_names_are_not_revived() {
        let now = Instant::now();
        let wall = Utc::now();
        let mut notices = ActivityNotices::default();
        notices.push(
            &event(DanmuEventKind::Enter, "正在欢迎", Some("1"), wall),
            now,
            wall,
        );
        notices.push(
            &event(DanmuEventKind::Like, "先点赞", Some("2"), wall),
            now,
            wall,
        );
        let at_two = text(&mut notices, now + MIN_READ_TIME, 120, true).unwrap();
        assert!(at_two.contains("正在欢迎"));
        assert!(at_two.contains("先点赞"));
        let at = wall + chrono::Duration::milliseconds(2100);
        notices.push(
            &event(DanmuEventKind::Like, "已过期", Some("3"), at),
            now + Duration::from_millis(2100),
            at,
        );
        let at = wall + chrono::Duration::seconds(4);
        notices.push(
            &event(DanmuEventKind::Enter, "新欢迎", Some("4"), at),
            now + Duration::from_secs(4),
            at,
        );
        // Both kinds are consumed in the new batch; hidden likes never become a later primary.
        let welcome = text(&mut notices, now + Duration::from_secs(4), 20, true).unwrap();
        assert!(welcome.contains("新欢迎"));
        assert!(!welcome.contains("点赞"));
        assert!(text(&mut notices, now + Duration::from_secs(9), 120, true).is_none());

        let mut stale = ActivityNotices::default();
        stale.push(
            &event(DanmuEventKind::Enter, "欢迎", Some("1"), wall),
            now,
            wall,
        );
        stale.push(
            &event(DanmuEventKind::Like, "旧名字", Some("2"), wall),
            now,
            wall,
        );
        let at = wall + chrono::Duration::seconds(4);
        stale.push(
            &event(DanmuEventKind::Like, "新名字", Some("3"), at),
            now + Duration::from_secs(4),
            at,
        );
        let rendered = text(&mut stale, now + Duration::from_secs(5), 120, true).unwrap();
        assert!(rendered.contains("新名字"));
        assert!(!rendered.contains("旧名字"));
    }

    #[test]
    fn fits_complete_names_before_truncation_and_respects_name_privacy() {
        let now = Instant::now();
        let wall = Utc::now();
        let mut notices = ActivityNotices::default();
        notices.push(
            &event(DanmuEventKind::Enter, "首位", Some("seed"), wall),
            now,
            wall,
        );
        for name in ["张三", "李四", "王五"] {
            notices.push(&event(DanmuEventKind::Enter, name, None, wall), now, wall);
        }
        let at = now + MIN_READ_TIME;
        let width = UnicodeWidthStr::width("张三、李四等 进入直播间");
        let fitted = text(&mut notices, at, width, true).unwrap();
        assert!(fitted.contains("张三、李四等"));
        assert!(!fitted.contains("王五"));
        assert!(!fitted.contains('…'));
        let hidden = text(&mut notices, at, 100, false).unwrap();
        for name in ["首位", "张三", "李四", "王五"] {
            assert!(!hidden.contains(name));
        }

        let mut long = ActivityNotices::default();
        long.push(
            &event(
                DanmuEventKind::Enter,
                "这是一个很长很长的观众昵称",
                Some("long"),
                wall,
            ),
            now,
            wall,
        );
        let shortened = text(&mut long, now, 20, true).unwrap();
        assert!(shortened.contains('…'));
        assert!(shortened.ends_with("进入直播间"));
        for width in [0, 1, 8, 20] {
            assert!(
                UnicodeWidthStr::width(text(&mut long, now, width, true).unwrap().as_str())
                    <= width
            );
        }
    }

    #[test]
    fn single_notifications_expire_and_history_or_late_events_do_not_restart_them() {
        let now = Instant::now();
        let wall = Utc::now();
        for (kind, seconds) in [(DanmuEventKind::Enter, 5), (DanmuEventKind::Like, 3)] {
            let mut notices = ActivityNotices::default();
            let mut incoming = event(kind, "观众", Some("1"), wall);
            notices.push(&incoming, now, wall);
            assert!(
                text(
                    &mut notices,
                    now + Duration::from_millis(seconds * 1000 - 1),
                    80,
                    true
                )
                .is_some()
            );
            assert!(text(&mut notices, now + Duration::from_secs(seconds), 80, true).is_none());
            notices.push(
                &incoming,
                now + Duration::from_secs(10),
                wall + chrono::Duration::seconds(10),
            );
            assert!(text(&mut notices, now + Duration::from_secs(10), 80, true).is_none());
            incoming.origin = DanmuEventOrigin::History;
            notices.push(&incoming, now, wall);
            assert!(text(&mut notices, now, 80, true).is_none());
        }
    }
}
