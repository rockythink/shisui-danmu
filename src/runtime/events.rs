use super::commands::{OperationState, OperationValue, ReviewedCandidate};
use crate::{
    bilibili::{AccountStatus, RoomSnapshot},
    domain::DanmuEvent,
    obs::{MicrophoneLevel, ObsStatus},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex, Notify, broadcast};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct EventRecord {
    pub message_key: u64,
    pub event: DanmuEvent,
    pub assistant_state: Option<crate::bridge::Mark>,
}
#[derive(Debug, Clone)]
pub struct CandidateRecord {
    pub expected: ReviewedCandidate,
    pub message_id: String,
    pub text: String,
    pub planned_segments: Vec<String>,
    pub reply_to: Option<ReplyTargetRecord>,
    pub state: crate::bridge::Execution,
    pub reason: String,
    pub order: u64,
    pub approval_required: bool,
    pub retry_of: Option<String>,
    pub diagnosis: Option<crate::delivery::Diagnosis>,
    pub confirmed_segments: Vec<String>,
    pub unconfirmed_segments: Vec<String>,
    pub duplicate_risk: bool,
}
#[derive(Debug, Clone)]
pub struct ReplyTargetRecord {
    pub user_id: String,
    pub username: String,
}
#[derive(Debug, Clone)]
pub struct SubscriptionAccountRecord {
    pub id: String,
    pub label: String,
    pub email: Option<String>,
    pub signed_in: bool,
    pub plan_enabled: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRecord {
    pub model_id: String,
    pub display_name: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStateRecord {
    NotRequested,
    Completed,
    Unavailable,
}

pub(super) fn search_state(value: crate::runner::model::SearchState) -> SearchStateRecord {
    match value {
        crate::runner::model::SearchState::NotRequested => SearchStateRecord::NotRequested,
        crate::runner::model::SearchState::Completed => SearchStateRecord::Completed,
        crate::runner::model::SearchState::Unavailable => SearchStateRecord::Unavailable,
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeStateRecord {
    pub session_id: String,
    pub room_id: String,
    pub room: Option<RoomSnapshot>,
    pub connection: String,
    pub watched: Option<u64>,
    pub likes: Option<u64>,
    pub online_viewers: Option<u64>,
    pub main_account: AccountStatus,
    pub assistant_account: AccountStatus,
    pub identity_generation: u64,
    pub assistant_running: bool,
    pub automatic_permission: bool,
    pub resume_pending: bool,
    pub assistant_note: String,
    pub assistant_failure: Option<String>,
    pub assistant_search: SearchStateRecord,
    pub assistant_returned_model: Option<String>,
    pub obs: Option<ObsStatus>,
    pub microphone_level: Option<MicrophoneLevel>,
    pub obs_error: Option<String>,
    pub stop_confirmation: Option<Uuid>,
    pub stop_seconds_remaining: Option<u64>,
    pub login_url: Option<String>,
    pub assistant_login_url: Option<String>,
    pub featured_message_id: Option<String>,
}
#[derive(Debug, Clone)]
pub struct RuntimeSnapshot {
    pub state: RuntimeStateRecord,
    pub events: Vec<EventRecord>,
    pub candidates: Vec<CandidateRecord>,
    pub deliveries: Vec<CandidateRecord>,
    pub new_message_count: u64,
}
#[derive(Debug, Clone)]
pub struct RuntimeDiagnostics {
    pub connection: String,
    pub assistant_note: String,
    pub assistant_details: String,
    pub history_warning: Option<String>,
    pub config_warning: Option<String>,
    pub obs_error: Option<String>,
    pub operations: Vec<(Uuid, OperationState)>,
}
#[derive(Debug, Clone)]
pub enum RuntimeChange {
    Upsert(EventRecord),
    Remove {
        message_key: u64,
    },
    State(Arc<RuntimeStateRecord>),
    Candidates(Vec<CandidateRecord>),
    Deliveries(Vec<CandidateRecord>),
    Operation {
        operation_id: Uuid,
        state: OperationState,
        value: OperationValue,
    },
    Notice {
        message: String,
        is_error: bool,
    },
    ResyncRequired,
}
#[derive(Debug, Clone)]
pub struct RuntimeBatch {
    pub sequence: u64,
    pub new_message_count: u64,
    pub snapshot: Option<RuntimeSnapshot>,
    pub changes: Vec<RuntimeChange>,
}
#[derive(Clone)]
pub(super) struct SequencedChange {
    pub sequence: u64,
    pub count: u64,
    pub change: RuntimeChange,
}

/// One consumer per subscription. A lag notification requires a fresh atomic observe().
pub struct RuntimeSubscription {
    pub(super) initial: Mutex<Option<RuntimeBatch>>,
    pub(super) receiver: Mutex<broadcast::Receiver<SequencedChange>>,
    pub(super) watermark: tokio::sync::watch::Receiver<(u64, u64)>,
    pub(super) closed: AtomicBool,
    pub(super) wake: Arc<Notify>,
}
impl RuntimeSubscription {
    pub async fn next_batch(&self) -> anyhow::Result<Option<RuntimeBatch>> {
        if self.closed.load(Ordering::Acquire) {
            return Ok(None);
        }
        if let Some(batch) = self.initial.lock().await.take() {
            return Ok(Some(batch));
        }
        let mut receiver = self.receiver.lock().await;
        let first = tokio::select! {
            biased;
            _ = self.wake.notified() => return Ok(None),
            value = receiver.recv() => value,
        };
        let first = match first {
            Ok(value) => value,
            Err(broadcast::error::RecvError::Closed) => return Ok(None),
            Err(broadcast::error::RecvError::Lagged(_)) => {
                return Ok(Some(self.resync_batch()));
            }
        };
        let mut batch = RuntimeBatch {
            sequence: first.sequence,
            new_message_count: first.count,
            snapshot: None,
            changes: vec![first.change],
        };
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(33);
        while batch.changes.len() < 64 {
            match receiver.try_recv() {
                Ok(value) => {
                    batch.sequence = value.sequence;
                    batch.new_message_count = value.count;
                    batch.changes.push(value.change);
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    batch = self.resync_batch();
                    break;
                }
                Err(broadcast::error::TryRecvError::Closed) => break,
                Err(broadcast::error::TryRecvError::Empty) => {
                    let next = tokio::select! {
                        biased;
                        _ = self.wake.notified() => break,
                        value = tokio::time::timeout_at(deadline, receiver.recv()) => value,
                    };
                    match next {
                        Ok(Ok(value)) => {
                            batch.sequence = value.sequence;
                            batch.new_message_count = value.count;
                            batch.changes.push(value.change);
                        }
                        Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                            batch = self.resync_batch();
                            break;
                        }
                        _ => break,
                    }
                }
            }
        }
        Ok(Some(batch))
    }
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.wake.notify_one();
    }
    fn resync_batch(&self) -> RuntimeBatch {
        self.close();
        let (sequence, new_message_count) = *self.watermark.borrow();
        RuntimeBatch {
            sequence,
            new_message_count,
            snapshot: None,
            changes: vec![RuntimeChange::ResyncRequired],
        }
    }
}

pub(super) fn candidate(reply: crate::bridge::Reply, generation: u64) -> CandidateRecord {
    CandidateRecord {
        expected: ReviewedCandidate {
            session: reply.session,
            caller: reply.caller,
            request_id: reply.request_id,
            revision: reply.revision,
            identity_generation: generation,
        },
        message_id: reply.message_id,
        planned_segments: super::assistant::assistant_segments(&reply.text),
        text: reply.text,
        reply_to: reply.reply_to.map(|target| ReplyTargetRecord {
            user_id: target.user_id,
            username: target.username,
        }),
        state: reply.state,
        reason: reply.reason,
        order: reply.order,
        approval_required: reply.approval_required,
        retry_of: reply.retry_of,
        diagnosis: reply.diagnosis,
        confirmed_segments: reply.confirmed_segments,
        unconfirmed_segments: reply.unconfirmed_segments,
        duplicate_risk: reply.duplicate_risk,
    }
}

impl CandidateRecord {
    pub(super) fn matches(&self, reply: &crate::bridge::Reply, generation: u64) -> bool {
        self.expected.identity_generation == generation
            && self.expected.session == reply.session
            && self.expected.caller == reply.caller
            && self.expected.request_id == reply.request_id
            && self.expected.revision == reply.revision
            && self.message_id == reply.message_id
            && self.text == reply.text
            && self
                .reply_to
                .as_ref()
                .map(|target| (&target.user_id, &target.username))
                == reply
                    .reply_to
                    .as_ref()
                    .map(|target| (&target.user_id, &target.username))
            && self.state == reply.state
            && self.reason == reply.reason
            && self.order == reply.order
            && self.approval_required == reply.approval_required
            && self.retry_of == reply.retry_of
            && self.confirmed_segments == reply.confirmed_segments
            && self.unconfirmed_segments == reply.unconfirmed_segments
            && self.duplicate_risk == reply.duplicate_risk
            && self.diagnosis.as_ref().map(diagnosis_fields)
                == reply.diagnosis.as_ref().map(diagnosis_fields)
    }
}

fn diagnosis_fields(
    value: &crate::delivery::Diagnosis,
) -> (
    crate::delivery::Cause,
    &str,
    bool,
    bool,
    Option<i64>,
    bool,
    bool,
) {
    (
        value.cause,
        &value.detail,
        value.response_received,
        value.acceptance_proof,
        value.platform_code,
        value.suspected_block,
        value.duplicate_risk,
    )
}

pub(super) fn obs_fields(
    value: &ObsStatus,
) -> (
    &str,
    crate::obs::StreamState,
    crate::obs::MicrophoneState,
    Option<&str>,
) {
    (
        &value.current_scene,
        value.stream,
        value.microphone,
        value.compatibility_warning.as_deref(),
    )
}
