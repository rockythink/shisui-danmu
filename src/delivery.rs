//! One queue for manual and generated messages. Model repair is a new authorized job, never a queue retry.
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Confirmed,
    Rejected,
    Uncertain,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    EchoConfirmed,
    EchoMissing,
    ContentRejected,
    OtherRejected,
    TransportUncertain,
    KnownWord,
    Cancelled,
}
#[derive(Debug, Clone, Serialize)]
pub struct Diagnosis {
    pub cause: Cause,
    pub detail: String,
    pub response_received: bool,
    pub acceptance_proof: bool,
    pub platform_code: Option<i64>,
    pub suspected_block: bool,
    pub duplicate_risk: bool,
}
impl Diagnosis {
    pub fn repairable(&self) -> bool {
        matches!(self.cause, Cause::ContentRejected | Cause::KnownWord)
    }
}
#[derive(Debug, Clone)]
pub struct Delivery {
    pub outcome: Outcome,
    pub diagnosis: Diagnosis,
    pub confirmed_segments: Vec<String>,
    pub unconfirmed_segments: Vec<String>,
}
impl Delivery {
    pub fn new(outcome: Outcome, cause: Cause, detail: impl Into<String>) -> Self {
        Self {
            outcome,
            diagnosis: Diagnosis {
                cause,
                detail: detail.into(),
                response_received: false,
                acceptance_proof: false,
                platform_code: None,
                suspected_block: cause == Cause::EchoMissing,
                duplicate_risk: matches!(cause, Cause::EchoMissing | Cause::TransportUncertain),
            },
            confirmed_segments: Vec::new(),
            unconfirmed_segments: Vec::new(),
        }
    }
}
impl From<Outcome> for Delivery {
    fn from(outcome: Outcome) -> Self {
        let (cause, detail) = match outcome {
            Outcome::Confirmed => (Cause::EchoConfirmed, "平台回显已确认"),
            Outcome::Rejected => (Cause::OtherRejected, "发送被拒绝；没有内容屏蔽证据"),
            Outcome::Uncertain => (
                Cause::TransportUncertain,
                "传输结果未知，可能已发送；不自动重发",
            ),
            Outcome::Cancelled => (Cause::Cancelled, "未发送部分已取消"),
        };
        Self::new(outcome, cause, detail)
    }
}

#[derive(Debug, Clone)]
pub struct Permit {
    cancelled: Arc<AtomicBool>,
    parent: Option<Arc<Permit>>,
    expires: DateTime<Utc>,
}
impl Permit {
    pub fn new(expires: DateTime<Utc>) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            parent: None,
            expires,
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
    pub fn child(&self, expires: DateTime<Utc>) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            parent: Some(Arc::new(self.clone())),
            expires: expires.min(self.expires),
        }
    }
    pub fn valid(&self) -> bool {
        !self.cancelled.load(Ordering::SeqCst)
            && Utc::now() < self.expires
            && self.parent.as_ref().is_none_or(|p| p.valid())
    }
}

pub trait Transport: Send + Sync {
    /// A real matching echo is required. A response/proof alone is never delivery confirmation.
    fn send_confirm(
        &self,
        text: &str,
        reply_to: Option<&str>,
    ) -> impl Future<Output = Delivery> + Send;
}

#[derive(Clone, Default)]
pub struct SendQueue {
    lock: Arc<Mutex<()>>,
    paused: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
}
impl SendQueue {
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
    pub async fn send<T: Transport>(
        &self,
        transport: &T,
        segments: &[String],
        permit: Option<&Permit>,
        reply_to: Option<&str>,
    ) -> Delivery {
        let generation = self.generation();
        let _guard = self.lock.lock().await;
        let mut confirmed = Vec::new();
        let mut last = None;
        for (index, segment) in segments.iter().enumerate() {
            let mut result = if self.is_paused()
                || self.generation() != generation
                || permit.is_some_and(|p| !p.valid())
            {
                Outcome::Cancelled.into()
            } else {
                // Do not abort an in-flight POST and guess whether it was sent.
                transport.send_confirm(segment, reply_to).await
            };
            if result.outcome != Outcome::Confirmed {
                if let Some(permit) = permit {
                    if !result.diagnosis.repairable()
                        || self.is_paused()
                        || self.generation() != generation
                    {
                        permit.cancel();
                    }
                } else {
                    // Drop this failure's queued generation, not future user submissions.
                    // An already-cancelled old job must not invalidate a newer generation.
                    let _ = self.generation.compare_exchange(
                        generation,
                        generation.wrapping_add(1),
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                }
                result.confirmed_segments = confirmed;
                result.unconfirmed_segments = segments[index..].to_vec();
                return result;
            }
            confirmed.push(segment.clone());
            last = Some(result);
            if index + 1 < segments.len() {
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            }
        }
        let mut result = last.unwrap_or_else(|| Outcome::Confirmed.into());
        result.confirmed_segments = confirmed;
        result
    }
}

#[cfg(test)]
mod tests;
