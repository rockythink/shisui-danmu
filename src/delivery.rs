//! One queue for manual and generated messages. No retries, including ambiguous HTTP outcomes.
use chrono::{DateTime, Utc};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Confirmed,
    Rejected,
    Uncertain,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct Permit {
    cancelled: Arc<AtomicBool>,
    expires: DateTime<Utc>,
}
impl Permit {
    pub fn new(expires: DateTime<Utc>) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            expires,
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
    pub fn child(&self, expires: DateTime<Utc>) -> Self {
        Self {
            cancelled: self.cancelled.clone(),
            expires,
        }
    }
    pub fn valid(&self) -> bool {
        !self.cancelled.load(Ordering::SeqCst) && Utc::now() < self.expires
    }
}

pub trait Transport: Send + Sync {
    /// Must return Confirmed only after platform acceptance AND a matching echo.
    fn send_confirm(&self, text: &str) -> impl std::future::Future<Output = Outcome> + Send;
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
    ) -> Outcome {
        let generation = self.generation();
        if self.is_paused() {
            return Outcome::Cancelled;
        }
        let _guard = self.lock.lock().await;
        for (index, segment) in segments.iter().enumerate() {
            if self.is_paused()
                || self.generation() != generation
                || permit.is_some_and(|permit| !permit.valid())
            {
                return Outcome::Cancelled;
            }
            // A request already in flight cannot be unsent. Never abort it and guess its result.
            let outcome = transport.send_confirm(segment).await;
            if outcome != Outcome::Confirmed {
                self.pause();
                return outcome;
            }
            if index + 1 < segments.len() {
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            }
        }
        Outcome::Confirmed
    }
}

#[cfg(test)]
mod tests;
