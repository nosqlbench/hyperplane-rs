// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `MockBarrierHandle` — in-memory implementation of
//! [`paramodel_executor::BarrierHandle`].
//!
//! Dependencies are registered up front. `mark_dependency_satisfied`
//! moves them from pending to satisfied; once the pending set is
//! empty the handle transitions to `Satisfied` and wakes any tasks
//! blocked on [`BarrierHandle::await_satisfied`].
//!
//! Uses a `tokio::sync::Notify` for waiters — suitable for async
//! tests but not a performance-optimised primitive.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jiff::Timestamp;
use paramodel_executor::{BarrierError, BarrierHandle, BarrierState};
use paramodel_plan::{BarrierId, StepId};
use tokio::sync::Notify;
use tokio::time::timeout;

/// In-memory reference implementation of [`BarrierHandle`].
pub struct MockBarrierHandle {
    id:           BarrierId,
    state:        Mutex<Inner>,
    notify:       Notify,
    created_at:   Instant,
}

impl std::fmt::Debug for MockBarrierHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.state.lock().expect("poisoned");
        f.debug_struct("MockBarrierHandle")
            .field("id", &self.id)
            .field("state", &inner.state)
            .field("pending", &inner.pending.len())
            .field("satisfied", &inner.satisfied.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Inner {
    state:         BarrierState,
    pending:       BTreeSet<StepId>,
    satisfied:     BTreeSet<StepId>,
    satisfied_at:  Option<Timestamp>,
    failed_reason: Option<String>,
    /// Once true, `await_with_timeout` short-circuits even on Pending.
    closed:        AtomicBool,
}

impl MockBarrierHandle {
    /// Build a handle that will satisfy when every dependency in
    /// `deps` is reported via [`BarrierHandle::mark_dependency_satisfied`].
    #[must_use]
    pub fn new(id: BarrierId, deps: BTreeSet<StepId>) -> Self {
        Self {
            id,
            state: Mutex::new(Inner {
                state:         if deps.is_empty() {
                    BarrierState::Satisfied
                } else {
                    BarrierState::Pending
                },
                pending:       deps,
                satisfied:     BTreeSet::new(),
                satisfied_at:  None,
                failed_reason: None,
                closed:        AtomicBool::new(false),
            }),
            notify: Notify::new(),
            created_at: Instant::now(),
        }
    }

    fn snapshot_state(&self) -> BarrierState {
        self.state.lock().expect("poisoned").state
    }
}

#[async_trait]
impl BarrierHandle for MockBarrierHandle {
    fn id(&self) -> &BarrierId {
        &self.id
    }

    fn state(&self) -> BarrierState {
        self.snapshot_state()
    }

    async fn await_satisfied(&self) -> Result<(), BarrierError> {
        loop {
            match self.snapshot_state() {
                BarrierState::Satisfied => return Ok(()),
                BarrierState::Failed => {
                    let reason = self
                        .state
                        .lock()
                        .expect("poisoned")
                        .failed_reason
                        .clone()
                        .unwrap_or_default();
                    return Err(BarrierError::Failed {
                        barrier_id: self.id.clone(),
                        reason,
                    });
                }
                BarrierState::TimedOut => {
                    return Err(BarrierError::TimedOut {
                        barrier_id: self.id.clone(),
                    });
                }
                BarrierState::Pending => {
                    self.notify.notified().await;
                }
            }
        }
    }

    async fn await_with_timeout(
        &self,
        budget: Duration,
    ) -> Result<bool, BarrierError> {
        match timeout(budget, self.await_satisfied()).await {
            Ok(Ok(())) => Ok(true),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // Mark TimedOut but don't fail — callers distinguish
                // via the `bool` return.
                let mut inner = self.state.lock().expect("poisoned");
                if matches!(inner.state, BarrierState::Pending) {
                    inner.state = BarrierState::TimedOut;
                    inner.closed.store(true, Ordering::SeqCst);
                }
                self.notify.notify_waiters();
                Ok(false)
            }
        }
    }

    async fn mark_dependency_satisfied(&self, step: &StepId) {
        let mut inner = self.state.lock().expect("poisoned");
        if inner.pending.remove(step) {
            inner.satisfied.insert(step.clone());
        }
        if matches!(inner.state, BarrierState::Pending) && inner.pending.is_empty() {
            inner.state = BarrierState::Satisfied;
            inner.satisfied_at = Some(Timestamp::now());
        }
        drop(inner);
        self.notify.notify_waiters();
    }

    async fn fail(&self, reason: &str) {
        let mut inner = self.state.lock().expect("poisoned");
        inner.state = BarrierState::Failed;
        inner.failed_reason = Some(reason.to_owned());
        inner.closed.store(true, Ordering::SeqCst);
        drop(inner);
        self.notify.notify_waiters();
    }

    async fn release(&self) {
        let mut inner = self.state.lock().expect("poisoned");
        inner.state = BarrierState::Satisfied;
        inner.satisfied_at = Some(Timestamp::now());
        inner.closed.store(true, Ordering::SeqCst);
        drop(inner);
        self.notify.notify_waiters();
    }

    fn satisfied_dependencies(&self) -> BTreeSet<StepId> {
        self.state.lock().expect("poisoned").satisfied.clone()
    }

    fn pending_dependencies(&self) -> BTreeSet<StepId> {
        self.state.lock().expect("poisoned").pending.clone()
    }

    fn wait_duration(&self) -> Option<Duration> {
        let inner = self.state.lock().expect("poisoned");
        match inner.state {
            BarrierState::Pending => Some(self.created_at.elapsed()),
            _ => None,
        }
    }

    fn satisfied_at(&self) -> Option<Timestamp> {
        self.state.lock().expect("poisoned").satisfied_at
    }
}
