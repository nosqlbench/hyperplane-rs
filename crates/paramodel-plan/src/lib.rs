// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Paramodel plan layer.
//!
//! Two halves:
//!
//! - **Authored side (SRD-0008)** — `TestPlan`, `Axis`,
//!   `ExecutionPolicies`, `OptimizationStrategy`, `TrialOrdering`,
//!   `PlanBindings`, and related identity types. This is what the
//!   user writes and what the compiler consumes.
//! - **Compiled side (SRD-0009)** — `AtomicStep`, `ExecutionGraph`,
//!   `ElementInstanceGraph`, and `ExecutionPlan` with compile-time
//!   metadata. This is what the compiler produces and the executor
//!   consumes.
//!
//! Compilation itself (how a `TestPlan` becomes an `ExecutionPlan`)
//! lives in SRD-0010's `paramodel-compiler` crate. This crate commits
//! only to the *shapes*.

pub mod axis;
pub mod bindings;
pub mod error;
pub mod execution;
pub mod ids;
pub mod instance;
pub mod ordering;
pub mod plan;
pub mod policies;
pub mod step;

pub use axis::Axis;
pub use bindings::PlanBindings;
pub use error::{PlanError, Result};
pub use execution::{
    CheckpointStrategy, ExecutionGraph, ExecutionPlan, ExecutionPlanId, ExecutionPlanMetadata,
    GraphStatistics, OptimizationReport, PerformanceMetrics, ResourceRequirements,
};
pub use ids::{AxisName, ElementParameterRef, PlanName, TestPlanId};
pub use instance::{
    ElementInstance, ElementInstanceGraph, InstanceDependency, InstanceId, InstanceScope,
};
pub use ordering::{OptimizationStrategy, TrialOrdering};
pub use plan::{TestPlan, TestPlanMetadata};
pub use policies::{
    BackoffStrategy, ExecutionPolicies, InterventionMode, OnFailure, PartialRunBehavior,
    RetryPolicy,
};
pub use step::{
    AtomicStep, BarrierId, BarrierKind, CheckpointId, OutputSelector, ShutdownReason, StepHeader,
    StepId, TimeoutAction,
};
