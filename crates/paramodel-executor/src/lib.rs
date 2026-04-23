// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

#![allow(
    clippy::significant_drop_tightening,
    reason = "V0.1 guards are short-lived; tightening buys nothing and clutters async fns"
)]

//! Paramodel executor layer (SRD-0011).
//!
//! Trait-first surface: `Executor`, `Orchestrator`, `Scheduler`,
//! `ResourceManager`, `ArtifactCollector`, `ExecutionStateManager`,
//! `JournalWriter`, `BarrierHandle`, plus the passive
//! `ExecutionObserver`. Each trait ships with a minimal reference
//! implementation suitable for tests and small runs:
//!
//! - [`DefaultExecutor`] — synchronous topological walk
//! - [`DefaultOrchestrator`] — pre-resolved runtime map
//! - [`DefaultScheduler`] — FIFO with cascading skip-on-failure
//! - [`DefaultResourceManager`] — unbounded
//! - [`InMemoryArtifactCollector`] — in-process buffer
//! - [`NoopStateManager`] — no persistence
//! - [`InMemoryJournalWriter`] — in-process log
//! - [`NoopObserver`] — no-op observer
//!
//! The persistence side (SRD-0012) wraps the state/journal/artifact
//! interfaces around real backends.

pub mod artifacts;
pub mod barrier;
pub mod error;
pub mod executor;
pub mod ids;
pub mod journal;
pub mod observer;
pub mod orchestrator;
pub mod resources;
pub mod scheduler;
pub mod state;

pub use artifacts::{
    ArtifactCollection, ArtifactCollector, ArtifactContent, ArtifactKind, CleanupReport,
    InMemoryArtifactCollector, RetentionPolicy,
};
pub use barrier::{BarrierHandle, BarrierState};
pub use error::{
    ArtifactError, BarrierError, ExecutionDiagnostic, ExecutionError, JournalError,
    OrchestratorError, ResourceError, SchedulerError,
};
pub use executor::{
    DefaultExecutor, ExecutionResults, ExecutionStatus, Executor, StepStatistics,
};
pub use ids::{AllocationId, ExecutionId, JournalSequence};
pub use journal::{
    InMemoryJournalWriter, JournalEvent, JournalEventKind, JournalWriter,
};
pub use observer::{ExecutionObserver, InterventionKind, NoopObserver};
pub use orchestrator::{DefaultOrchestrator, Orchestrator, StepOutcome};
pub use resources::{
    DefaultResourceManager, PoolPriority, QuotaUsage, ResourceAllocation, ResourceCapacity,
    ResourceManager, ResourcePool, ResourceQuota, ResourceRequest, ResourceSnapshot,
    ResourceUsage,
};
pub use scheduler::{
    DefaultScheduler, Scheduler, SchedulerPriority, SchedulerState, SchedulerStatistics,
    SchedulingPolicy,
};
pub use state::{
    Checkpoint, CheckpointHandle, CheckpointState, ExecutionStateManager, IdempotencyClass,
    InFlightResolution, InMemoryStateManager, NoopStateManager, RecoveryResult,
};
