// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Paramodel — the parameter-modelling and trial-orchestration system.
//!
//! This crate is a thin facade that re-exports every paramodel
//! sub-crate as a module. Depend on `paramodel` alone to get access to
//! the whole system; depend on individual `paramodel-*` crates when
//! you want a smaller compilation unit.
//!
//! Modules:
//!
//! - [`elements`] — the central algebra (SRD-0003–0007): parameters,
//!   values, constraints, expressions, elements, attributes,
//!   element runtime, trials.
//! - [`plan`] — authored test plans and compiled execution plans
//!   (SRD-0008, SRD-0009).
//! - [`trials`] — trial sets, trial status, trial results
//!   (SRD-0006).
//! - [`compiler`] — reducto-style compilation from `TestPlan` to
//!   `ExecutionPlan` (SRD-0010).
//! - [`executor`] — execution engine, orchestrator, scheduler,
//!   resource manager, and friends (SRD-0011).

pub use paramodel_compiler as compiler;
pub use paramodel_elements as elements;
pub use paramodel_executor as executor;
pub use paramodel_plan as plan;
pub use paramodel_trials as trials;
