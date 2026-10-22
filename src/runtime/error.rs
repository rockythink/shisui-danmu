//! Stable reasons at the public host boundary, without changing its anyhow API.
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFailureKind {
    InvalidInput,
    Busy,
    Stale,
    PermissionDenied,
    Storage,
    Transport,
    Protocol,
    Closed,
}

/// `message` is a user-facing safe summary, never a request body or credential.
/// Keeping this as an explicit anyhow cause lets hosts classify known guards
/// without parsing localized text or exposing the full diagnostic chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFailure {
    pub kind: RuntimeFailureKind,
    pub message: String,
}

impl RuntimeFailure {
    pub fn new(kind: RuntimeFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for RuntimeFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RuntimeFailure {}
