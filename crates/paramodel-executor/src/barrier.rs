// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `BarrierHandle` trait + `BarrierState`.
//!
//! V0.1 ships only the trait surface and a minimal `NoopBarrierHandle`
//! that simulates "always already satisfied" — real barriers arrive
//! alongside a full parallel executor in a follow-up slice.

use std::collections::BTreeSet;
use std::time::Duration;

use async_trait::async_trait;
use jiff::Timestamp;
use paramodel_plan::{BarrierId, StepId};
use serde::{Deserialize, Serialize};

use crate::error::BarrierError;

/// Runtime state of one barrier handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarrierState {
    /// Not yet satisfied.
    Pending,
    /// All dependencies completed.
    Satisfied,
    /// Forced into failure state.
    Failed,
    /// Waited past the timeout.
    TimedOut,
}

/// Runtime handle paired 1:1 with a plan-layer
/// `AtomicStep::Barrier` via `barrier_id`.
#[async_trait]
pub trait BarrierHandle: Send + Sync + 'static {
    /// Barrier identifier.
    fn id(&self) -> &BarrierId;

    /// Current state.
    fn state(&self) -> BarrierState;

    /// Block until the barrier reaches `Satisfied`. Errors out on
    /// `Failed` / `TimedOut`.
    async fn await_satisfied(&self) -> Result<(), BarrierError>;

    /// Bounded wait; returns `true` iff satisfied within `timeout`.
    async fn await_with_timeout(&self, timeout: Duration) -> Result<bool, BarrierError>;

    /// Scheduler / state manager informs the handle that one
    /// dependency is now in terminal state.
    async fn mark_dependency_satisfied(&self, step: &StepId);

    /// Force the handle into `Failed`.
    async fn fail(&self, reason: &str);

    /// Force the handle into `Satisfied` regardless of dependencies.
    async fn release(&self);

    /// Dependencies already satisfied.
    fn satisfied_dependencies(&self) -> BTreeSet<StepId>;

    /// Dependencies still waiting.
    fn pending_dependencies(&self) -> BTreeSet<StepId>;

    /// How long the handle has been waiting.
    fn wait_duration(&self) -> Option<Duration>;

    /// When satisfaction was recorded.
    fn satisfied_at(&self) -> Option<Timestamp>;
}
