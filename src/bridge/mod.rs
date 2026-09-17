//! The TUI owns this state. CLI and MCP only talk to its local authenticated endpoint.
pub mod mcp;
pub(crate) mod routing;
pub mod wire;

use crate::{
    delivery::{Delivery, Diagnosis, Outcome, Permit},
    domain::DanmuEvent,
};
use anyhow::{Result, ensure};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Notify;
use uuid::Uuid;

pub(crate) const RUNNER_CALLER: &str = "danmu-assistant";
pub(crate) const AI_PREFIX: &str = "✦ ";
const HISTORY: usize = 512;
const REQUESTS: usize = 4096;
fn batch() -> usize {
    50
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Status,
    Messages {
        session: String,
        #[serde(default)]
        cursor: u64,
        #[serde(default = "batch")]
        limit: usize,
        #[serde(default)]
        wait_ms: u64,
    },
    Report {
        session: String,
        caller: String,
        request_id: String,
        message_id: String,
        state: ReportState,
    },
    Reply {
        session: String,
        caller: String,
        request_id: String,
        message_id: String,
        text: String,
        #[serde(default)]
        candidate: bool,
    },
    Result {
        session: String,
        caller: String,
        request_id: String,
    },
}
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReportState {
    Processing,
    Finished,
    Failed,
}
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mark {
    Processing,
    Confirmed,
    Finished,
    Failed,
}
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Execution {
    AwaitingApproval,
    Accepted,
    Sending,
    Confirmed,
    Uncertain,
    Rejected,
    Cancelled,
}

#[derive(Clone, Serialize)]
pub struct Message {
    pub cursor: u64,
    #[serde(flatten)]
    pub event: DanmuEvent,
}
#[derive(Clone, Serialize, PartialEq, Eq)]
pub struct ReplyTarget {
    pub user_id: String,
    pub username: String,
}
#[derive(Clone, Serialize)]
pub struct Reply {
    pub session: String,
    pub caller: String,
    pub request_id: String,
    pub message_id: String,
    pub text: String,
    pub revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<ReplyTarget>,
    pub state: Execution,
    pub reason: String,
    pub order: u64,
    pub approval_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnosis: Option<Diagnosis>,
    pub confirmed_segments: Vec<String>,
    pub unconfirmed_segments: Vec<String>,
    pub duplicate_risk: bool,
    #[serde(skip)]
    owner: Option<String>,
    #[serde(skip)]
    authorization: Option<Permit>,
    #[serde(skip)]
    original_message: Option<DanmuEvent>,
    #[serde(skip)]
    allow_repair: bool,
}
impl Reply {
    pub(crate) fn original_message(&self) -> Option<&DanmuEvent> {
        self.original_message.as_ref()
    }
}
#[derive(Clone)]
pub(crate) struct Repair {
    pub original: Reply,
    pub delivery: Delivery,
    pub permit: Permit,
}
impl Repair {
    pub fn original_message(&self) -> Option<&DanmuEvent> {
        self.original.original_message.as_ref()
    }
}
struct Stored {
    fingerprint: String,
    response: Value,
}
struct State {
    session: String,
    active: bool,
    available: bool,
    granted: bool,
    permit: Permit,
    sequence: u64,
    order: u64,
    messages: VecDeque<Message>,
    reports: HashMap<String, Mark>,
    outcomes: HashMap<String, Mark>,
    requests: HashMap<(String, String), Stored>,
    replies: HashMap<(String, String), Reply>,
    local: bool,
    driver: Option<String>,
    repair_enabled: bool,
    blocked_words: Vec<String>,
    repairs: VecDeque<Repair>,
    inbox: Option<routing::Inbox>,
    routing_active: bool,
    leased: Vec<Message>,
}
#[derive(Clone)]
pub struct Bridge {
    state: tokio::sync::watch::Sender<State>,
    changed: Arc<Notify>,
}
pub struct Job {
    pub reply: Reply,
    pub permit: Permit,
}
impl Default for Bridge {
    fn default() -> Self {
        Self::new(false)
    }
}
impl Bridge {
    fn initial(local: bool) -> State {
        State {
            session: Uuid::new_v4().to_string(),
            active: true,
            available: local,
            granted: false,
            permit: Permit::new(Utc::now() + Duration::days(1)),
            sequence: 0,
            order: 0,
            messages: VecDeque::new(),
            reports: HashMap::new(),
            outcomes: HashMap::new(),
            requests: HashMap::new(),
            replies: HashMap::new(),
            local,
            driver: None,
            repair_enabled: true,
            blocked_words: Vec::new(),
            repairs: VecDeque::new(),
            inbox: None,
            routing_active: false,
            leased: Vec::new(),
        }
    }
    pub fn new(local: bool) -> Self {
        let (state, _) = tokio::sync::watch::channel(Self::initial(local));
        Self {
            state,
            changed: Arc::new(Notify::new()),
        }
    }
    fn mutate<R>(&self, change: impl FnOnce(&mut State) -> R) -> R {
        let mut change = Some(change);
        let mut result = None;
        self.state
            .send_modify(|state| result = Some(change.take().unwrap()(state)));
        result.unwrap()
    }

    pub fn session_id(&self) -> String {
        self.state.borrow().session.clone()
    }
    pub fn is_local(&self) -> bool {
        self.state.borrow().local
    }
    pub fn is_active(&self) -> bool {
        self.state.borrow().active
    }
    pub fn sending_enabled(&self) -> bool {
        self.state.borrow().granted
    }
    pub fn status(&self) -> Value {
        let s = self.state.borrow();
        json!({"session":s.session,"active":s.active,"available":s.available,"sending_enabled":s.granted,
            "local_transport":s.local,"driver":if s.driver.is_some(){"runner"}else{"external"},"cursor":s.sequence,"oldest_cursor":s.messages.front().map(|m|m.cursor),
            "history_capacity":HISTORY,"request_capacity":REQUESTS,"routing":Self::routing_snapshot_state(&s),"version":env!("CARGO_PKG_VERSION")})
    }
    pub fn mark(&self, id: &str) -> Option<Mark> {
        let state = self.state.borrow();
        state
            .outcomes
            .get(id)
            .or_else(|| state.reports.get(id))
            .copied()
    }

    /// Current-scene conversation only, newest first; never advances a consumer cursor.
    pub(crate) fn recent_conversation_before(
        &self,
        session: &str,
        before: u64,
    ) -> Result<Vec<DanmuEvent>> {
        let state = self.state.borrow();
        Self::session(&state, session)?;
        ensure!(state.active, "session_ended");
        Ok(state
            .messages
            .iter()
            .rev()
            .filter(|message| {
                message.cursor < before
                    && matches!(
                        message.event.kind,
                        crate::domain::DanmuEventKind::Danmu
                            | crate::domain::DanmuEventKind::Superchat
                    )
            })
            .take(12)
            .map(|message| message.event.clone())
            .collect())
    }

    pub fn ingest(&self, event: DanmuEvent) {
        self.mutate(|s| {
            if !s.active || s.messages.iter().any(|m| m.event.id == event.id) {
                return;
            }
            s.sequence += 1;
            let cursor = s.sequence;
            let message = Message { cursor, event };
            if s.routing_active
                && let Some(inbox) = s.inbox.as_mut()
            {
                inbox.push(&message, Instant::now());
            }
            s.messages.push_back(message);
            while s.messages.len() > HISTORY {
                if let Some(old) = s.messages.pop_front() {
                    s.reports.remove(&old.event.id);
                    s.outcomes.remove(&old.event.id);
                }
            }
            self.changed.notify_waiters();
        })
    }
    fn revoke(s: &mut State, reason: &str) {
        s.granted = false;
        s.permit.cancel();
        // Revoke old dispatch authorization, not the user's unapproved candidates.
        for reply in s
            .replies
            .values_mut()
            .filter(|r| r.state == Execution::Accepted)
        {
            reply.state = Execution::Cancelled;
            reply.reason = reason.into();
            s.reports.insert(reply.message_id.clone(), Mark::Finished);
        }
    }
    /// A sender or context switch invalidates approvals, not reviewed drafts or real POST results.
    pub(crate) fn identity_changed(&self) {
        self.invalidate_approvals(
            "发送账号已变化，须按新身份重新审核",
            "发送账号已变化，旧自动发送授权失效",
        );
    }
    pub(crate) fn context_changed(&self) {
        self.invalidate_approvals(
            "选中资料已变化或读取失败，须重新审核候选",
            "选中资料已变化或读取失败，旧自动发送授权失效",
        );
    }
    fn invalidate_approvals(&self, review_reason: &str, cancel_reason: &str) {
        self.mutate(|s| {
            s.granted = false;
            s.permit.cancel();
            s.repairs.clear();
            for reply in s.replies.values_mut() {
                match reply.state {
                    Execution::AwaitingApproval => {
                        reply.authorization = None;
                        reply.revision = reply.revision.saturating_add(1);
                        reply.reason = review_reason.into();
                    }
                    Execution::Accepted if reply.approval_required => {
                        reply.state = Execution::AwaitingApproval;
                        reply.authorization = None;
                        reply.revision = reply.revision.saturating_add(1);
                        reply.reason = review_reason.into();
                    }
                    Execution::Accepted => {
                        reply.state = Execution::Cancelled;
                        reply.reason = cancel_reason.into();
                        s.reports.insert(reply.message_id.clone(), Mark::Finished);
                    }
                    _ => {}
                }
            }
            self.changed.notify_waiters();
        });
    }
    pub fn permission(&self, enabled: bool) -> Result<()> {
        self.mutate(|s| {
            if enabled {
                ensure!(s.active && s.available, "本场未就绪，不能授权");
                if s.granted && s.permit.valid() {
                    return Ok(());
                }
                s.permit.cancel();
                s.permit = Permit::new(Utc::now() + Duration::days(1));
                s.granted = true;
            } else {
                Self::revoke(s, "TUI撤销本场Agent发送许可；在途请求继续确认");
            }
            self.changed.notify_waiters();
            Ok(())
        })
    }
    pub fn available(&self, available: bool) {
        self.mutate(|s| {
            if s.available && !available {
                Self::revoke(s, "平台连接中断；重连必须重新授权，不补发");
            }
            s.available = available;
        })
    }
    pub fn end(&self) {
        self.mutate(|s| {
            Self::revoke(s, "本场已结束");
            for reply in s
                .replies
                .values_mut()
                .filter(|r| r.state == Execution::AwaitingApproval)
            {
                reply.state = Execution::Cancelled;
                reply.reason = "本场已结束".into();
                s.reports.insert(reply.message_id.clone(), Mark::Finished);
            }
            for (id, mark) in &mut s.reports {
                if *mark == Mark::Processing
                    && !s
                        .replies
                        .values()
                        .any(|reply| &reply.message_id == id && reply.state == Execution::Sending)
                {
                    *mark = Mark::Finished;
                }
            }
            Self::invalidate_routing(s);
            s.active = false;
            s.available = false;
            self.changed.notify_waiters();
        })
    }
    pub fn new_session(&self) {
        self.mutate(|s| {
            Self::revoke(s, "新场次");
            *s = Self::initial(s.local);
        });
        self.changed.notify_waiters();
    }

    pub(crate) fn candidate_count(&self) -> usize {
        self.state
            .borrow()
            .replies
            .values()
            .filter(|reply| reply.state == Execution::AwaitingApproval)
            .count()
    }

    pub(crate) fn selected_candidate(&self, selected: Option<&(String, String)>) -> Option<Reply> {
        let state = self.state.borrow();
        selected
            .and_then(|key| state.replies.get(key))
            .filter(|reply| reply.state == Execution::AwaitingApproval)
            .or_else(|| {
                state
                    .replies
                    .values()
                    .filter(|reply| reply.state == Execution::AwaitingApproval)
                    .min_by_key(|reply| reply.order)
            })
            .cloned()
    }

    pub fn candidates(&self) -> Vec<Reply> {
        let s = self.state.borrow();
        let mut replies: Vec<_> = s
            .replies
            .values()
            .filter(|r| r.state == Execution::AwaitingApproval)
            .cloned()
            .collect();
        replies.sort_by_key(|r| r.order);
        replies
    }

    pub(crate) fn edit_candidate(
        &self,
        session: &str,
        caller: &str,
        request: &str,
        text: &str,
    ) -> Result<()> {
        self.mutate(|s| {
            Self::session(s, session)?;
            ensure!(s.active, "session_ended");
            let reply = s
                .replies
                .get_mut(&(caller.into(), request.into()))
                .ok_or_else(|| anyhow::anyhow!("候选不存在"))?;
            ensure!(
                reply.state == Execution::AwaitingApproval,
                "候选已结束，不能保存编辑"
            );
            Self::validate_reply_text(text)?;
            // Keep the original request fingerprint: retries must return this edited reply.
            reply.revision = reply
                .revision
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("候选版本已耗尽"))?;
            reply.text = text.to_owned();
            Ok(())
        })
    }

    pub(crate) fn validate_reply_text(text: &str) -> Result<()> {
        ensure!(
            !text.trim().is_empty()
                && text.len() <= 4096
                && text.chars().all(Self::reply_char_allowed),
            "回复须非空、最多4096字节且无控制字符"
        );
        Ok(())
    }

    fn reply_char_allowed(c: char) -> bool {
        !c.is_control()
            && !matches!(
                c,
                '\u{200b}'..='\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2060}'..='\u{206f}'
                    | '\u{feff}'
            )
    }

    fn native_user_id(message: &DanmuEvent) -> Option<&str> {
        message.author_id.as_deref().filter(|user_id| {
            user_id.bytes().all(|b| b.is_ascii_digit())
                && user_id.parse::<u64>().is_ok_and(|id| id != 0)
        })
    }

    /// Validate an explicit TUI selection atomically; never grant automatic sending.
    pub(crate) fn approve_candidates(&self, expected: &[Reply]) -> Result<()> {
        self.mutate(|s| {
            ensure!(!expected.is_empty(), "暂无待审回复");
            ensure!(s.active && s.available, "本场未就绪，不能发送候选");
            let keys: Vec<_> = expected
                .iter()
                .map(|reply| (reply.caller.clone(), reply.request_id.clone()))
                .collect();
            for (expected, key) in expected.iter().zip(&keys) {
                Self::session(s, &expected.session)?;
                let reply = s
                    .replies
                    .get(key)
                    .ok_or_else(|| anyhow::anyhow!("候选不存在"))?;
                ensure!(
                    reply.state == Execution::AwaitingApproval
                        && reply.revision == expected.revision
                        && reply.message_id == expected.message_id
                        && reply.order == expected.order
                        && reply.text == expected.text
                        && reply.reply_to == expected.reply_to,
                    "候选内容或对象已变化，请重新查看后批准"
                );
                ensure!(
                    reply.authorization.as_ref().is_none_or(Permit::valid),
                    "原发送授权已失效，不能重新批准改写"
                );
            }
            if !s.permit.valid() {
                s.permit = Permit::new(Utc::now() + Duration::days(1));
            }
            for key in keys {
                let reply = s.replies.get_mut(&key).unwrap();
                if reply.authorization.is_none() {
                    reply.authorization = Some(s.permit.child(Utc::now() + Duration::minutes(10)));
                }
                reply.approval_required = true;
                reply.state = Execution::Accepted;
                reply.reason = "TUI已批准本条，尚未发送".into();
            }
            self.changed.notify_waiters();
            Ok(())
        })
    }
    pub fn decide(&self, caller: &str, request: &str, approve: bool) -> Result<()> {
        self.mutate(|s| {
            if approve {
                ensure!(
                    s.active && s.available && s.granted && s.permit.valid(),
                    "本场Agent发送许可已暂停"
                );
            }
            let reply = s
                .replies
                .get_mut(&(caller.into(), request.into()))
                .ok_or_else(|| anyhow::anyhow!("候选不存在"))?;
            ensure!(
                reply.state == Execution::AwaitingApproval,
                "候选已结束，不能重复批准"
            );
            ensure!(
                !approve || reply.authorization.as_ref().is_none_or(Permit::valid),
                "原发送授权已失效，不能重新批准改写"
            );
            reply.state = if approve {
                Execution::Accepted
            } else {
                Execution::Cancelled
            };
            reply.reason = if approve {
                "TUI已批准，尚未发送"
            } else {
                "TUI丢弃候选"
            }
            .into();
            let id = reply.message_id.clone();
            if !approve {
                s.reports.insert(id, Mark::Finished);
            }
            Ok(())
        })
    }
    pub fn take_ready(&self) -> Option<Job> {
        self.mutate(|s| {
            if !s.active || !s.available || !s.permit.valid() {
                return None;
            }
            let key = s
                .replies
                .iter()
                .filter(|(_, r)| {
                    r.state == Execution::Accepted
                        && (s.granted || r.authorization.as_ref().is_some_and(Permit::valid))
                })
                .min_by_key(|(_, r)| r.order)
                .map(|(k, _)| k.clone())?;
            let r = s.replies.get_mut(&key).unwrap();
            let permit = r
                .authorization
                .get_or_insert_with(|| s.permit.child(Utc::now() + Duration::minutes(10)))
                .clone();
            if !permit.valid() {
                r.state = Execution::Cancelled;
                r.reason = "原发送授权已失效".into();
                return None;
            }
            r.state = Execution::Sending;
            r.reason = "正在执行；尚未确认".into();
            Some(Job {
                reply: r.clone(),
                permit,
            })
        })
    }
    pub fn complete(&self, job: &Reply, delivery: Delivery) {
        let outcome = delivery.outcome;
        self.mutate(|s| {
            if s.session != job.session {
                return;
            }
            let Some(reply) = s
                .replies
                .get_mut(&(job.caller.clone(), job.request_id.clone()))
            else {
                return;
            };
            if reply.state != Execution::Sending {
                return;
            }
            reply.state = match outcome {
                Outcome::Confirmed => Execution::Confirmed,
                Outcome::Uncertain => Execution::Uncertain,
                Outcome::Rejected => Execution::Rejected,
                Outcome::Cancelled => Execution::Cancelled,
            };
            reply.reason = delivery.diagnosis.detail.clone();
            reply.diagnosis = Some(delivery.diagnosis.clone());
            reply.confirmed_segments = delivery.confirmed_segments.clone();
            reply.unconfirmed_segments = delivery.unconfirmed_segments.clone();
            reply.duplicate_risk |= delivery.diagnosis.duplicate_risk;
            let repairable = delivery.diagnosis.repairable()
                && reply.allow_repair
                && reply.owner.is_some()
                && reply.owner == s.driver
                && s.repair_enabled
                && reply.retry_of.is_none()
                && !delivery.unconfirmed_segments.is_empty()
                && s.active
                && s.available
                && reply.authorization.as_ref().is_some_and(Permit::valid);
            if repairable {
                s.repairs.push_back(Repair {
                    original: reply.clone(),
                    delivery: delivery.clone(),
                    permit: reply.authorization.clone().unwrap(),
                });
            }
            if outcome == Outcome::Confirmed {
                s.outcomes.insert(job.message_id.clone(), Mark::Confirmed);
            } else if outcome == Outcome::Uncertain
                && s.outcomes.get(&job.message_id) != Some(&Mark::Confirmed)
            {
                s.outcomes.insert(job.message_id.clone(), Mark::Failed);
            }
            let mark = match outcome {
                Outcome::Confirmed => Mark::Confirmed,
                Outcome::Cancelled => Mark::Finished,
                _ => Mark::Failed,
            };
            if matches!(outcome, Outcome::Uncertain | Outcome::Rejected)
                && !repairable
                && !(outcome == Outcome::Uncertain
                    && delivery.diagnosis.cause == crate::delivery::Cause::EchoMissing)
            {
                Self::revoke(s, "发送失败或未知；撤销Agent许可，不重试，不影响真人");
            }
            if s.reports.get(&job.message_id) != Some(&Mark::Confirmed) {
                s.reports.insert(job.message_id.clone(), mark);
            }
            self.changed.notify_waiters();
        })
    }
    pub(crate) fn owned_policy(&self, driver: &str, repair_enabled: bool, words: &[String]) {
        self.mutate(|s| {
            if s.driver.as_deref() == Some(driver) {
                s.repair_enabled = repair_enabled;
                if s.blocked_words != words {
                    s.blocked_words = words.to_vec();
                }
                if !repair_enabled {
                    s.repairs.clear();
                }
            }
        });
    }
    pub(crate) fn job_authorized(&self, reply: &Reply) -> bool {
        let s = self.state.borrow();
        s.session == reply.session
            && s.active
            && s.available
            && reply.authorization.as_ref().is_some_and(Permit::valid)
    }
    pub(crate) fn blocked_word(&self, reply: &Reply, text: &str) -> Option<String> {
        let s = self.state.borrow();
        reply.owner.as_ref()?;
        s.blocked_words
            .iter()
            .find(|word| text.contains(word.as_str()))
            .cloned()
    }
    pub(crate) fn take_repair(&self, driver: &str) -> Option<Repair> {
        self.mutate(|s| {
            if s.driver.as_deref() != Some(driver) || !s.repair_enabled {
                return None;
            }
            while let Some(repair) = s.repairs.pop_front() {
                if s.active
                    && s.available
                    && repair.permit.valid()
                    && repair.original.owner.as_deref() == Some(driver)
                {
                    return Some(repair);
                }
            }
            None
        })
    }
    pub(crate) fn apply_repair(
        &self,
        driver: &str,
        repair: &Repair,
        text: String,
        automatic: bool,
    ) -> Result<Value> {
        self.mutate(|s| {
            ensure!(
                s.driver.as_deref() == Some(driver)
                    && repair.original.owner.as_deref() == Some(driver),
                "改写归属失效"
            );
            Self::session(s, &repair.original.session)?;
            ensure!(
                s.active && s.available && s.repair_enabled && repair.permit.valid(),
                "改写的原授权已失效"
            );
            ensure!(
                repair.original.retry_of.is_none()
                    && !s
                        .replies
                        .values()
                        .any(|r| r.retry_of.as_deref() == Some(&repair.original.request_id)),
                "每条候选最多改写一次"
            );
            Self::validate_reply_text(&text)?;
            let body = |s: &str| {
                s.trim()
                    .trim_start_matches(AI_PREFIX.trim())
                    .trim()
                    .to_owned()
            };
            let original = repair
                .delivery
                .unconfirmed_segments
                .iter()
                .map(|s| body(s))
                .collect::<Vec<_>>()
                .join("");
            ensure!(body(&text) != original, "改写未改变失败正文，停止且不重发");
            ensure!(
                !repair
                    .delivery
                    .confirmed_segments
                    .iter()
                    .any(|segment| text.contains(body(segment).as_str())),
                "改写重复已确认分段，停止且不重发"
            );
            ensure!(s.requests.len() < REQUESTS, "本场请求记录已满");
            let mut reply = repair.original.clone();
            reply.request_id = Uuid::new_v4().to_string();
            reply.retry_of = Some(repair.original.request_id.clone());
            reply.text = text;
            reply.approval_required |= !automatic;
            reply.state = if reply.approval_required {
                Execution::AwaitingApproval
            } else {
                Execution::Accepted
            };
            reply.reason = if reply.approval_required {
                "合规改写待再次批准"
            } else {
                "合规改写一次，尚未送达"
            }
            .into();
            reply.confirmed_segments.clear();
            reply.unconfirmed_segments.clear();
            reply.authorization = Some(repair.permit.clone());
            s.order += 1;
            reply.order = s.order;
            let response = serde_json::to_value(&reply)?;
            let key = (reply.caller.clone(), reply.request_id.clone());
            s.requests.insert(
                key.clone(),
                Stored {
                    fingerprint: String::new(),
                    response: response.clone(),
                },
            );
            s.replies.insert(key, reply);
            Ok(response)
        })
    }
    pub(crate) fn reply_state(&self, expected: &Reply) -> Option<Execution> {
        let s = self.state.borrow();
        if s.session != expected.session {
            return None;
        }
        s.replies
            .get(&(expected.caller.clone(), expected.request_id.clone()))
            .filter(|reply| reply.order == expected.order)
            .map(|reply| reply.state)
    }
    pub(crate) fn late_echo(&self, job: &Reply) {
        self.mutate(|s| {
            if s.session != job.session {
                return;
            }
            if let Some(permit) = &job.authorization {
                permit.cancel();
            }
            for reply in s
                .replies
                .values_mut()
                .filter(|r| r.retry_of.as_deref() == Some(&job.request_id))
            {
                match reply.state {
                    Execution::AwaitingApproval | Execution::Accepted => {
                        reply.state = Execution::Cancelled;
                        reply.reason = "原段迟到回显；改写未送出，已取消".into();
                    }
                    Execution::Sending | Execution::Confirmed | Execution::Uncertain => {
                        reply.duplicate_risk = true;
                    }
                    _ => {}
                }
            }
            // The original terminal Result is immutable; observation only cancels its repair authority.
        });
    }
    fn session(s: &State, session: &str) -> Result<()> {
        ensure!(
            s.session == session,
            "session_expired: 本场标识失效，请重新读取状态；不得补发旧回复"
        );
        Ok(())
    }
    fn identity(value: &str) -> Result<()> {
        ensure!(
            !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control),
            "标识必须为1..128字节且无控制字符"
        );
        Ok(())
    }
    fn original_message<'a>(
        s: &'a State,
        id: &str,
        from_runner: bool,
        stored_reply: bool,
    ) -> Option<&'a DanmuEvent> {
        if from_runner && let Some(message) = s.leased.iter().find(|message| message.event.id == id)
        {
            return Some(&message.event);
        }
        s.messages
            .iter()
            .find(|message| message.event.id == id)
            .map(|message| &message.event)
            .or_else(|| {
                if from_runner && stored_reply {
                    s.replies.values().find_map(|reply| {
                        (reply.message_id == id && reply.owner.as_deref() == s.driver.as_deref())
                            .then_some(reply.original_message.as_ref())
                            .flatten()
                    })
                } else {
                    None
                }
            })
    }
    fn message<'a>(
        s: &'a State,
        id: &str,
        from_runner: bool,
        stored_reply: bool,
    ) -> Result<&'a DanmuEvent> {
        Self::original_message(s, id, from_runner, stored_reply)
            .ok_or_else(|| anyhow::anyhow!("message_unavailable: 消息不在当前可见历史中"))
    }
    pub fn apply(&self, req: Request) -> Result<Value> {
        if matches!(req, Request::Status) {
            return Ok(self.status());
        }
        self.mutate(|s| {
            ensure!(s.driver.is_none() || !matches!(&req, Request::Reply { caller, .. } | Request::Report { caller, .. } if caller == RUNNER_CALLER), "runner_active: 同一自动助手已有所有者；其他caller不受此互斥限制");
            Self::apply_state(s, req, false, false, true)
        })
    }
    pub(crate) fn claim_driver(&self, driver: &str) -> Result<()> {
        self.mutate(|s| {
            ensure!(s.active && s.available, "本场未就绪");
            ensure!(s.driver.is_none(), "已有本场助手驱动");
            ensure!(
                !s.replies
                    .values()
                    .any(|r| r.caller == RUNNER_CALLER && r.state == Execution::Sending),
                "同一助手在途发送未完成"
            );
            for reply in s
                .replies
                .values_mut()
                .filter(|r| r.caller == RUNNER_CALLER && r.state == Execution::Accepted)
            {
                reply.state = Execution::Cancelled;
                reply.reason = "同一助手切换所有权".into();
            }
            Self::invalidate_routing(s);
            s.driver = Some(driver.to_owned());
            Ok(())
        })
    }
    pub(crate) fn release_driver(&self, driver: &str) {
        self.mutate(|s| {
            if s.driver.as_deref() == Some(driver) {
                // Ownership release is not a global exclusion of other callers.
                Self::invalidate_routing(s);
                s.driver = None;
            }
        })
    }
    fn invalidate_routing(s: &mut State) {
        if let Some(inbox) = s.inbox.as_mut() {
            inbox.clear();
        }
        s.routing_active = false;
        s.leased.clear();
    }

    fn routing_snapshot_state(s: &State) -> Value {
        json!({
            "active": s.routing_active,
            "queue": s.inbox.as_ref().map(routing::Inbox::snapshot),
            "leased": s.leased.iter().map(|message| message.event.id.as_str()).collect::<Vec<_>>(),
        })
    }

    pub(crate) fn start_routing(
        &self,
        driver: &str,
        policy: routing::Policy,
        visible: bool,
    ) -> Result<()> {
        self.mutate(|s| {
            ensure!(
                s.driver.as_deref() == Some(driver),
                "runner_expired: 助手归属已失效"
            );
            ensure!(s.active && s.available, "本场未就绪");
            let mut inbox = routing::Inbox::new(policy);
            if visible {
                let now = Instant::now();
                for message in &s.messages {
                    inbox.push(message, now);
                }
            }
            s.inbox = Some(inbox);
            s.routing_active = true;
            s.leased.clear();
            Ok(())
        })
    }
    pub(crate) fn maintain_routing(&self, driver: &str) {
        self.mutate(|s| {
            if s.driver.as_deref() == Some(driver)
                && s.routing_active
                && let Some(inbox) = &mut s.inbox
            {
                inbox.maintain(Instant::now());
            }
        });
    }

    pub(crate) fn update_routing(&self, driver: &str, policy: routing::Policy) -> Result<()> {
        self.mutate(|s| {
            ensure!(
                s.driver.as_deref() == Some(driver),
                "runner_expired: 助手归属已失效"
            );
            ensure!(
                s.active && s.available && s.routing_active,
                "分流队列未运行"
            );
            s.inbox
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("分流队列未运行"))?
                .configure(policy);
            Ok(())
        })
    }

    pub(crate) fn stop_routing(&self, driver: &str) {
        self.mutate(|s| {
            if s.driver.as_deref() == Some(driver) {
                Self::invalidate_routing(s);
            }
        })
    }

    pub(crate) fn routing_models(&self, driver: &str) -> Result<Vec<Message>> {
        self.mutate(|s| {
            ensure!(
                s.driver.as_deref() == Some(driver),
                "runner_expired: 助手归属已失效"
            );
            ensure!(
                s.active && s.available && s.routing_active,
                "分流队列未运行"
            );
            Ok(s.inbox
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("分流队列未运行"))?
                .models(Instant::now()))
        })
    }

    pub(crate) fn consume_routing_models(&self, driver: &str, ids: &[String]) -> Result<()> {
        self.mutate(|s| {
            ensure!(
                s.driver.as_deref() == Some(driver),
                "runner_expired: 助手归属已失效"
            );
            ensure!(
                s.active && s.available && s.routing_active,
                "分流队列未运行"
            );
            ensure!(s.leased.is_empty(), "上一批模型消息尚未结束");
            ensure!(!ids.is_empty() && ids.len() <= 8, "模型消息批次须为1..8条");
            let inbox = s
                .inbox
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("分流队列未运行"))?;
            let visible = inbox.models(Instant::now());
            let mut leased = Vec::with_capacity(ids.len());
            for (index, id) in ids.iter().enumerate() {
                ensure!(!ids[..index].contains(id), "模型消息ID重复");
                let message = visible
                    .iter()
                    .find(|message| message.event.id == *id)
                    .ok_or_else(|| anyhow::anyhow!("模型消息已失效"))?;
                leased.push(message.clone());
            }
            inbox.consume_models(ids);
            s.leased = leased;
            Ok(())
        })
    }

    pub(crate) fn expire_routing_flight(&self, driver: &str) -> Result<()> {
        self.mutate(|s| {
            ensure!(
                s.driver.as_deref() == Some(driver),
                "runner_expired: 助手归属已失效"
            );
            let ids = s
                .leased
                .iter()
                .map(|message| message.event.id.clone())
                .collect::<Vec<_>>();
            s.inbox
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("分流队列未运行"))?
                .record_expired(&ids);
            Ok(())
        })
    }

    pub(crate) fn finish_routing_models(&self, driver: &str) {
        self.mutate(|s| {
            if s.driver.as_deref() == Some(driver) {
                s.leased.clear();
            }
        })
    }

    pub(crate) fn drop_routing_model(&self, driver: &str, id: &str, reason: &str) -> Result<()> {
        self.mutate(|s| {
            ensure!(
                s.driver.as_deref() == Some(driver),
                "runner_expired: 助手归属已失效"
            );
            ensure!(s.routing_active, "分流队列未运行");
            let inbox = s
                .inbox
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("分流队列未运行"))?;
            ensure!(inbox.get(id).is_some(), "模型消息不存在");
            inbox.drop_model(id, reason);
            Ok(())
        })
    }

    pub(crate) fn routing_snapshot(&self) -> Value {
        Self::routing_snapshot_state(&self.state.borrow())
    }

    pub(crate) fn routing_has_priority(&self) -> bool {
        let state = self.state.borrow();
        state.active
            && state.available
            && state.routing_active
            && state
                .inbox
                .as_ref()
                .is_some_and(routing::Inbox::has_priority)
    }

    pub(crate) fn routing_backlog(&self) -> usize {
        self.state
            .borrow()
            .replies
            .values()
            .filter(|reply| {
                reply.caller == RUNNER_CALLER
                    && matches!(
                        reply.state,
                        Execution::AwaitingApproval | Execution::Accepted | Execution::Sending
                    )
            })
            .count()
    }

    pub(crate) fn submit_routing_template(&self, driver: &str, automatic: bool) -> Result<bool> {
        self.mutate(|s| {
            ensure!(
                s.driver.as_deref() == Some(driver),
                "runner_expired: 助手归属已失效"
            );
            ensure!(
                s.active && s.available && s.routing_active,
                "分流队列未运行"
            );
            if s.replies.values().any(|reply| {
                reply.caller == RUNNER_CALLER
                    && matches!(
                        reply.state,
                        Execution::AwaitingApproval | Execution::Accepted | Execution::Sending
                    )
            }) {
                return Ok(false);
            }
            if automatic && !(s.granted && s.permit.valid()) {
                return Ok(false);
            }
            let now = Instant::now();
            let template = match s
                .inbox
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("分流队列未运行"))?
                .template(now)
            {
                Some(template) => template,
                None => return Ok(false),
            };

            let template_id = template.message.event.id.clone();
            let mut text = template.text;
            if template.mention_sender
                && Self::native_user_id(&template.message.event).is_none()
                && let Some(username) = template.message.event.username.as_deref()
            {
                let username = username
                    .chars()
                    .filter(|c| {
                        Self::reply_char_allowed(*c) && !matches!(*c, '\u{2028}' | '\u{2029}')
                    })
                    .take(64)
                    .collect::<String>();
                let username = username.trim();
                if !username.is_empty() {
                    text = format!("@{username} {text}");
                }
            }
            // Borrow the exact batch source first even if an event ID was reused.
            s.leased.insert(0, template.message);
            let request = Request::Reply {
                session: s.session.clone(),
                caller: RUNNER_CALLER.into(),
                request_id: Uuid::new_v4().to_string(),
                message_id: template_id.clone(),
                text,
                candidate: !automatic,
            };
            let submitted = Self::apply_state(s, request, true, template.mention_sender, false);
            s.leased.remove(0);
            submitted?;
            s.inbox
                .as_mut()
                .unwrap()
                .consume_template(&template_id, now);
            Ok(true)
        })
    }

    pub(crate) fn apply_owned(&self, driver: &str, req: Request) -> Result<Value> {
        ensure!(
            matches!(&req, Request::Report { .. }),
            "助手状态接口只接受Report"
        );
        self.apply_driver(driver, req, false)
    }
    pub(crate) fn apply_owned_reply(
        &self,
        driver: &str,
        req: Request,
        mention_sender: bool,
    ) -> Result<Value> {
        ensure!(
            matches!(&req, Request::Reply { .. }),
            "助手回复接口只接受Reply"
        );
        self.apply_driver(driver, req, mention_sender)
    }
    fn apply_driver(&self, driver: &str, req: Request, mention_sender: bool) -> Result<Value> {
        self.mutate(|s| {
            ensure!(s.driver.as_deref() == Some(driver), "runner_expired: 助手归属已失效");
            ensure!(matches!(&req, Request::Reply {caller,..} | Request::Report {caller,..} if caller == RUNNER_CALLER), "助手不能代替其他caller写入");
            Self::apply_state(s, req, true, mention_sender, true)
        })
    }
    pub(crate) fn recent_results(&self) -> Vec<Reply> {
        let s = self.state.borrow();
        let mut replies: Vec<_> = s
            .replies
            .values()
            .filter(|r| {
                matches!(
                    r.state,
                    Execution::Confirmed | Execution::Uncertain | Execution::Rejected
                )
            })
            .collect();
        replies.sort_by_key(|r| std::cmp::Reverse(r.order));
        replies.into_iter().take(8).cloned().collect()
    }
    fn apply_state(
        s: &mut State,
        req: Request,
        from_runner: bool,
        mention_sender: bool,
        allow_repair: bool,
    ) -> Result<Value> {
        let fingerprint = if matches!(req, Request::Report { .. } | Request::Reply { .. }) {
            serde_json::to_string(&req)?
        } else {
            String::new()
        };

        match req {
            Request::Status => unreachable!(),
            Request::Messages {
                session,
                cursor,
                limit,
                wait_ms,
            } => {
                Self::session(s, &session)?;
                ensure!(
                    (1..=200).contains(&limit) && wait_ms <= 25_000,
                    "limit须1..200，wait_ms须0..25000"
                );
                ensure!(cursor <= s.sequence, "cursor_invalid: 游标超出当前场次");
                let oldest = s.messages.front().map(|m| m.cursor).unwrap_or(1);
                let messages: Vec<_> = s
                    .messages
                    .iter()
                    .filter(|m| m.cursor > cursor)
                    .take(limit)
                    .collect();
                let next = messages.last().map(|m| m.cursor).unwrap_or(cursor);
                Ok(
                    json!({"session":s.session,"active":s.active,"messages":messages,"cursor":next,"latest_cursor":s.sequence,"gap":cursor.saturating_add(1)<oldest,"oldest_cursor":oldest}),
                )
            }
            Request::Result {
                session,
                caller,
                request_id,
            } => {
                Self::session(s, &session)?;
                if let Some(r) = s.replies.get(&(caller.clone(), request_id.clone())) {
                    return Ok(serde_json::to_value(r)?);
                }
                Ok(s.requests
                    .get(&(caller, request_id))
                    .ok_or_else(|| anyhow::anyhow!("request_not_found"))?
                    .response
                    .clone())
            }
            Request::Report {
                session,
                caller,
                request_id,
                message_id,
                state,
            } => {
                Self::session(s, &session)?;
                Self::identity(&caller)?;
                Self::identity(&request_id)?;
                let key = (caller, request_id);
                if let Some(old) = s.requests.get(&key) {
                    ensure!(old.fingerprint == fingerprint, "idempotency_conflict");
                    return Ok(old.response.clone());
                }
                ensure!(s.active, "session_ended");
                Self::message(s, &message_id, from_runner, true)?;
                ensure!(
                    s.requests.len() < REQUESTS,
                    "request_capacity: 本场请求记录已满；不驱逐幂等记录"
                );
                let mark = match state {
                    ReportState::Processing => Mark::Processing,
                    ReportState::Finished => Mark::Finished,
                    ReportState::Failed => Mark::Failed,
                };
                // Generation reports must not replace an existing pending/sending reply.
                // Actual platform outcomes retain the higher priority in mark().
                let live_reply = from_runner
                    && s.replies.values().any(|reply| {
                        reply.message_id == message_id
                            && matches!(
                                reply.state,
                                Execution::AwaitingApproval
                                    | Execution::Accepted
                                    | Execution::Sending
                            )
                    });
                if !live_reply && s.reports.get(&message_id) != Some(&Mark::Confirmed) {
                    s.reports.insert(message_id.clone(), mark);
                }
                let response = json!({"reported":state,"message_id":message_id});
                s.requests.insert(
                    key,
                    Stored {
                        fingerprint,
                        response: response.clone(),
                    },
                );
                Ok(response)
            }
            Request::Reply {
                session,
                caller,
                request_id,
                message_id,
                text,
                candidate,
            } => {
                Self::session(s, &session)?;
                Self::identity(&caller)?;
                Self::identity(&request_id)?;
                let key = (caller.clone(), request_id.clone());
                if let Some(old) = s.requests.get(&key) {
                    ensure!(old.fingerprint == fingerprint, "idempotency_conflict");
                    return Ok(serde_json::to_value(
                        s.replies
                            .get(&key)
                            .ok_or_else(|| anyhow::anyhow!("request_kind_conflict"))?,
                    )?);
                }
                ensure!(s.active, "session_ended");
                let original_message = Self::message(s, &message_id, from_runner, false)?.clone();
                Self::validate_reply_text(&text)?;
                ensure!(
                    s.requests.len() < REQUESTS,
                    "request_capacity: 本场请求记录已满"
                );
                // Bind before review. Retries above preserve this snapshot despite settings
                // changes, text edits, or the source leaving the bounded history.
                let reply_to = if mention_sender {
                    Self::native_user_id(&original_message).map(|user_id| ReplyTarget {
                        user_id: user_id.into(),
                        username: original_message.username.clone().unwrap_or_default(),
                    })
                } else {
                    None
                };
                let allowed =
                    s.available && ((from_runner && candidate) || (s.granted && s.permit.valid()));
                let state = if !allowed {
                    Execution::Rejected
                } else if candidate {
                    Execution::AwaitingApproval
                } else {
                    Execution::Accepted
                };
                s.order += 1;
                let reply = Reply {
                    session,
                    caller,
                    request_id,
                    message_id: message_id.clone(),
                    text,
                    reply_to,
                    approval_required: candidate,
                    retry_of: None,
                    diagnosis: None,
                    confirmed_segments: Vec::new(),
                    unconfirmed_segments: Vec::new(),
                    duplicate_risk: false,
                    owner: if from_runner { s.driver.clone() } else { None },
                    revision: 0,
                    authorization: None,
                    original_message: Some(original_message),
                    allow_repair,
                    state,
                    order: s.order,
                    reason: if allowed {
                        "仅受理，不代表发送成功"
                    } else {
                        "本场Agent发送未授权或暂停；不会自动补发"
                    }
                    .into(),
                };
                let response = serde_json::to_value(&reply)?;
                s.requests.insert(
                    key.clone(),
                    Stored {
                        fingerprint,
                        response: response.clone(),
                    },
                );
                s.replies.insert(key, reply);
                if s.reports.get(&message_id) != Some(&Mark::Confirmed) {
                    s.reports.insert(
                        message_id,
                        if allowed {
                            Mark::Processing
                        } else {
                            Mark::Failed
                        },
                    );
                }
                Ok(response)
            }
        }
    }

    pub async fn call(&self, req: Request) -> Result<Value> {
        if let Request::Messages { wait_ms, .. } = &req {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(*wait_ms);
            loop {
                let wake = self.changed.notified();
                tokio::pin!(wake);
                wake.as_mut().enable();
                let value = self.apply(req.clone())?;
                if !value["messages"].as_array().unwrap().is_empty()
                    || value["active"] == false
                    || *wait_ms == 0
                {
                    return Ok(value);
                }
                if tokio::time::timeout_at(deadline, wake).await.is_err() {
                    return self.apply(req);
                }
            }
        }
        self.apply(req)
    }
}

#[cfg(test)]
mod tests;
