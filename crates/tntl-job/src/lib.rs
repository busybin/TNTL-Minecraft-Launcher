//! Cancellable jobs and progress events shared by every long operation.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JobId(pub u64);

impl JobId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Pending,
    Running,
    Done,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobEvent {
    pub job: JobId,
    pub stage: String,
    pub done: u64,
    pub total: Option<u64>,
    pub message: Option<String>,
}

impl JobEvent {
    pub fn stage(job: JobId, stage: impl Into<String>) -> Self {
        Self {
            job,
            stage: stage.into(),
            done: 0,
            total: None,
            message: None,
        }
    }

    pub fn progress(job: JobId, stage: impl Into<String>, done: u64, total: u64) -> Self {
        Self {
            job,
            stage: stage.into(),
            done,
            total: Some(total),
            message: None,
        }
    }
}

/// Cheap cloneable cancellation flag. Jobs poll it between units of work.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_token_is_shared() {
        let token = CancelToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.is_cancelled());
    }

    #[test]
    fn job_ids_are_unique() {
        assert_ne!(JobId::next(), JobId::next());
    }
}
