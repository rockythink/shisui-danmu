use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DanmuEventKind {
    Danmu,
    Gift,
    #[serde(rename = "guard")]
    GuardEvent,
    Superchat,
    Enter,
    Like,
    Follow,
    Share,
    Pk,
    Lottery,
    Moderation,
    RoomStatus,
    System,
}

impl DanmuEventKind {
    pub fn activity_lifetime(self) -> Option<Duration> {
        match self {
            Self::Enter => Some(Duration::seconds(5)),
            Self::Like => Some(Duration::seconds(3)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum DanmuEventOrigin {
    #[default]
    Live,
    History,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DanmuEmote {
    pub text: String,
    #[serde(default)]
    pub fallback: String,
    pub image_url: url::Url,
    pub width: Option<u32>,
    pub height: Option<u32>,
    #[serde(default)]
    pub is_animated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlindGift {
    pub name: String,
    pub reveal_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GiftDetails {
    pub name: String,
    pub action: String,
    pub blind_gift: Option<BlindGift>,
    pub receipts: HashMap<String, u64>,
    pub reported_quantity: u64,
}

impl GiftDetails {
    fn merge(&mut self, update: Self) -> bool {
        let mut changed = false;
        for (id, quantity) in update.receipts {
            let current = self.receipts.entry(id).or_default();
            if quantity > *current {
                *current = quantity;
                changed = true;
            }
        }
        if update.reported_quantity > self.reported_quantity {
            self.reported_quantity = update.reported_quantity;
            changed = true;
        }
        changed
    }

    pub fn content(&self) -> String {
        // Individual receipts and batch totals describe the same gifts, not two additions.
        let quantity = self
            .receipts
            .values()
            .fold(0_u64, |sum, count| sum.saturating_add(*count))
            .max(self.reported_quantity);
        if let Some(blind) = &self.blind_gift {
            format!(
                "开启 {} ×{}，{} {} ×{}",
                blind.name, quantity, blind.reveal_action, self.name, quantity
            )
        } else {
            format!("{} {} ×{}", self.action, self.name, quantity)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DanmuEvent {
    pub id: String,
    pub kind: DanmuEventKind,
    pub timestamp: DateTime<Utc>,
    pub username: Option<String>,
    pub author_id: Option<String>,
    pub content: String,
    #[serde(default)]
    pub origin: DanmuEventOrigin,
    pub platform_event_id: Option<String>,
    #[serde(default)]
    pub emotes: Vec<DanmuEmote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gift: Option<Box<GiftDetails>>,
}

impl DanmuEvent {
    pub fn new(kind: DanmuEventKind, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            kind,
            timestamp: Utc::now(),
            username: None,
            author_id: None,
            content: content.into(),
            origin: DanmuEventOrigin::Live,
            platform_event_id: None,
            emotes: Vec::new(),
            gift: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetrics {
    pub total_event_count: u64,
    pub event_counts: HashMap<DanmuEventKind, u64>,
}

impl SessionMetrics {
    pub fn count(&self, kind: DanmuEventKind) -> u64 {
        self.event_counts.get(&kind).copied().unwrap_or_default()
    }

    fn record(&mut self, event: &DanmuEvent) {
        self.total_event_count += 1;
        *self.event_counts.entry(event.kind).or_default() += 1;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DanmuSessionStatus {
    Active,
    Interrupted,
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DanmuSessionEndReason {
    Completed,
    Replaced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DanmuSession {
    pub id: String,
    pub room_id: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub status: DanmuSessionStatus,
    pub end_reason: Option<DanmuSessionEndReason>,
    pub recent_events: Vec<DanmuEvent>,
    pub metrics: SessionMetrics,
    pub featured_event: Option<DanmuEvent>,
    pub event_limit: usize,
    #[serde(skip)]
    seen_event_ids: HashSet<String>,
}

impl DanmuSession {
    pub fn new(room_id: impl Into<String>) -> Self {
        Self::with_options(room_id, 240, Utc::now())
    }

    pub fn with_options(
        room_id: impl Into<String>,
        event_limit: usize,
        started_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            room_id: room_id.into(),
            started_at,
            ended_at: None,
            status: DanmuSessionStatus::Active,
            end_reason: None,
            recent_events: Vec::new(),
            metrics: SessionMetrics::default(),
            featured_event: None,
            event_limit: event_limit.max(1),
            seen_event_ids: HashSet::new(),
        }
    }

    pub fn ingest(&mut self, event: DanmuEvent) -> bool {
        if self.status != DanmuSessionStatus::Active {
            return false;
        }
        if self.seen_event_ids.contains(&event.id) {
            let Some(position) = self
                .recent_events
                .iter()
                .position(|existing| existing.id == event.id)
            else {
                return false;
            };
            if event.kind != DanmuEventKind::Gift
                || self.recent_events[position].kind != DanmuEventKind::Gift
            {
                return false;
            }
            let existing = &mut self.recent_events[position];
            match (&mut existing.gift, event.gift) {
                (Some(current), Some(update)) => {
                    if !current.merge(*update) {
                        return false;
                    }
                    existing.content = current.content();
                }
                (None, Some(update)) => {
                    existing.content = update.content();
                    existing.gift = Some(update);
                }
                _ => return false,
            }
            if self
                .featured_event
                .as_ref()
                .is_some_and(|featured| featured.id == event.id)
            {
                self.featured_event = Some(existing.clone());
            }
            return true;
        }
        self.seen_event_ids.insert(event.id.clone());
        let position = self
            .recent_events
            .partition_point(|existing| existing.timestamp >= event.timestamp);
        self.recent_events.insert(position, event.clone());
        // Short-lived activity must not consume the retained chat history quota.
        if self.recent_events.len() > self.event_limit {
            let mut message_count = 0;
            let mut activity_count = 0;
            self.recent_events.retain(|event| {
                let count = if event.kind.activity_lifetime().is_some() {
                    &mut activity_count
                } else {
                    &mut message_count
                };
                *count += 1;
                *count <= self.event_limit
            });
        }
        self.metrics.record(&event);
        true
    }

    pub fn interrupt(&mut self) {
        if self.status == DanmuSessionStatus::Active {
            self.status = DanmuSessionStatus::Interrupted;
        }
    }

    pub fn resume(&mut self) {
        if self.status == DanmuSessionStatus::Interrupted {
            self.status = DanmuSessionStatus::Active;
        }
    }
    pub fn restore_runtime_state(&mut self) {
        self.seen_event_ids = self
            .recent_events
            .iter()
            .map(|event| event.id.clone())
            .collect();
    }

    pub fn end(&mut self, at: DateTime<Utc>, reason: DanmuSessionEndReason) {
        if self.status == DanmuSessionStatus::Ended {
            return;
        }
        self.status = DanmuSessionStatus::Ended;
        self.ended_at = Some(at.max(self.started_at));
        self.end_reason = Some(reason);
    }

    pub fn feature(&mut self, event_id: Option<&str>) {
        self.featured_event = event_id.and_then(|id| self.event(id).cloned());
    }

    fn event(&self, event_id: &str) -> Option<&DanmuEvent> {
        self.recent_events
            .iter()
            .find(|event| event.id == event_id)
            .or_else(|| {
                self.featured_event
                    .as_ref()
                    .filter(|event| event.id == event_id)
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcasterIdentity {
    pub nickname: String,
    pub author_id: Option<String>,
}

impl BroadcasterIdentity {
    pub fn new(nickname: Option<&str>, author_id: Option<&str>) -> Option<Self> {
        let nickname = nonempty(nickname?)?;
        Some(Self {
            nickname,
            author_id: author_id.and_then(nonempty),
        })
    }

    pub fn matches(&self, event: &DanmuEvent) -> bool {
        if event.kind != DanmuEventKind::Danmu {
            return false;
        }
        if let (Some(expected), Some(actual)) = (
            &self.author_id,
            event.author_id.as_deref().and_then(usable_id),
        ) {
            return actual == *expected;
        }
        event
            .username
            .as_deref()
            .and_then(nonempty)
            .is_some_and(|name| {
                name.eq_ignore_ascii_case(&self.nickname)
                    || name.to_lowercase() == self.nickname.to_lowercase()
            })
    }

    pub fn canonicalize(&self, event: &DanmuEvent) -> DanmuEvent {
        if !self.matches(event) || event.username.as_deref() == Some(&self.nickname) {
            return event.clone();
        }
        let mut canonical = event.clone();
        canonical.username = Some(self.nickname.clone());
        if canonical.author_id.is_none() {
            canonical.author_id = self.author_id.clone();
        }
        canonical
    }
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn usable_id(value: &str) -> Option<String> {
    nonempty(value).filter(|value| value != "0")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryState {
    Idle,
    AwaitingEcho {
        message: String,
        broadcaster_nickname: String,
        submitted_at: DateTime<Utc>,
    },
    Confirmed {
        event_id: String,
    },
    TimedOut {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DanmuDeliveryTracker {
    pub state: DeliveryState,
}

impl Default for DanmuDeliveryTracker {
    fn default() -> Self {
        Self {
            state: DeliveryState::Idle,
        }
    }
}

impl DanmuDeliveryTracker {
    pub fn begin(&mut self, message: &str, broadcaster_nickname: &str, at: DateTime<Utc>) {
        self.state = DeliveryState::AwaitingEcho {
            message: message.trim().to_owned(),
            broadcaster_nickname: broadcaster_nickname.trim().to_owned(),
            submitted_at: at,
        };
    }

    pub fn receive(&mut self, event: &DanmuEvent) -> bool {
        let DeliveryState::AwaitingEcho {
            message,
            broadcaster_nickname,
            ..
        } = &self.state
        else {
            return false;
        };
        if event.kind != DanmuEventKind::Danmu
            || event.username.as_deref().is_none_or(|name| {
                !name
                    .trim()
                    .eq_ignore_ascii_case(broadcaster_nickname.trim())
            })
            || event.content.trim() != message.trim()
        {
            return false;
        }
        self.state = DeliveryState::Confirmed {
            event_id: event.id.clone(),
        };
        true
    }

    pub fn expire(&mut self, at: DateTime<Utc>, timeout: Duration) -> bool {
        let DeliveryState::AwaitingEcho {
            message,
            submitted_at,
            ..
        } = &self.state
        else {
            return false;
        };
        if at - *submitted_at < timeout.max(Duration::zero()) {
            return false;
        }
        self.state = DeliveryState::TimedOut {
            message: message.clone(),
        };
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentRoomHistory {
    pub room_ids: Vec<String>,
    pub limit: usize,
}

impl RecentRoomHistory {
    pub fn new(room_ids: impl IntoIterator<Item = String>, limit: usize) -> Self {
        let limit = limit.max(1);
        let mut seen = HashSet::new();
        let room_ids = room_ids
            .into_iter()
            .map(|room| room.trim().to_owned())
            .filter(|room| !room.is_empty() && seen.insert(room.clone()))
            .take(limit)
            .collect();
        Self { room_ids, limit }
    }

    pub fn record(&mut self, room_id: &str) {
        let room_id = room_id.trim();
        if room_id.is_empty() {
            return;
        }
        self.room_ids.retain(|existing| existing != room_id);
        self.room_ids.insert(0, room_id.to_owned());
        self.room_ids.truncate(self.limit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, kind: DanmuEventKind, content: &str) -> DanmuEvent {
        DanmuEvent {
            id: id.into(),
            kind,
            timestamp: Utc::now(),
            username: Some("viewer".into()),
            author_id: Some("1".into()),
            content: content.into(),
            origin: DanmuEventOrigin::Live,
            platform_event_id: None,
            emotes: Vec::new(),
            gift: None,
        }
    }

    #[test]
    fn session_keeps_recent_projection_without_question_metadata() {
        let mut session = DanmuSession::with_options("1", 2, Utc::now());
        assert!(session.ingest(event("q", DanmuEventKind::Danmu, "为什么？")));
        for index in 0..5 {
            assert!(session.ingest(event(&format!("e{index}"), DanmuEventKind::Danmu, "好的",)));
        }
        assert_eq!(session.metrics.total_event_count, 6);
        assert_eq!(session.recent_events.len(), 2);
        let snapshot = serde_json::to_value(session).unwrap();
        assert!(snapshot.get("questionRecords").is_none());
        assert!(snapshot.get("questionClassifier").is_none());
    }

    #[test]
    fn transient_activity_does_not_evict_chat_history() {
        let mut session = DanmuSession::with_options("1", 2, Utc::now());
        session.ingest(event("older", DanmuEventKind::Danmu, "早些时候的弹幕"));
        session.ingest(event("newer", DanmuEventKind::Danmu, "最新弹幕"));
        for index in 0..6 {
            let kind = if index % 2 == 0 {
                DanmuEventKind::Enter
            } else {
                DanmuEventKind::Like
            };
            session.ingest(event(&format!("activity-{index}"), kind, "临时通知"));
        }
        let chats = session
            .recent_events
            .iter()
            .filter(|event| event.kind == DanmuEventKind::Danmu)
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(chats, ["newer", "older"]);
        assert!(session.recent_events.len() <= 4);
        assert_eq!(session.metrics.total_event_count, 8);
        assert!(!session.ingest(event("activity-5", DanmuEventKind::Like, "重复通知")));
        session.ingest(event("latest", DanmuEventKind::Danmu, "后来弹幕"));
        assert!(
            !session
                .recent_events
                .iter()
                .any(|event| event.id == "older")
        );
        assert!(
            session
                .recent_events
                .iter()
                .any(|event| event.id == "newer")
        );
    }
    #[test]
    fn legacy_event_defaults_origin_and_emotes() {
        let value = serde_json::json!({
            "id":"1", "kind":"danmu", "timestamp":"2026-09-01T00:00:00Z",
            "username":"u", "authorID":"1", "content":"hi", "platformEventID":null
        });
        let decoded: DanmuEvent = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.origin, DanmuEventOrigin::Live);
        assert!(decoded.emotes.is_empty());
    }

    #[test]
    fn session_orders_messages_by_timestamp_not_arrival_order() {
        let mut session = DanmuSession::new("1");
        let now = Utc::now();
        let mut newest = event("new", DanmuEventKind::Danmu, "最新");
        newest.timestamp = now;
        let mut delayed_history = event("old", DanmuEventKind::Danmu, "较早");
        delayed_history.timestamp = now - Duration::minutes(1);

        assert!(session.ingest(newest));
        assert!(session.ingest(delayed_history));
        assert_eq!(session.recent_events[0].content, "最新");
        assert_eq!(session.recent_events[1].content, "较早");
    }

    #[test]
    fn recent_rooms_are_trimmed_deduplicated_and_bounded() {
        let mut rooms = RecentRoomHistory::new(vec![" 1 ".into(), "2".into(), "1".into()], 2);
        assert_eq!(rooms.room_ids, vec!["1", "2"]);
        rooms.record("3");
        assert_eq!(rooms.room_ids, vec!["3", "1"]);
    }
}
