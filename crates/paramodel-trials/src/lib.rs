// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Paramodel trial-set and trial-result layer.
//!
//! `TrialSet` — an immutable collection of `Trial`s (from
//! `paramodel-elements`) together with the `SamplingStrategy` that
//! produced them. Storage is `Vec<Trial>` for reproducibility and
//! reporting, **not** for execution ordering (see SRD-0006 D5;
//! inter-element ordering lives on `RelationshipType::Linear`,
//! SRD-0007 D4).
//!
//! `TrialStatus`, `TrialResult` and supporting types record the
//! outcome after execution. `Trial`, `Assignments`, `TrialMetadata`,
//! and `Trial::canonical_bytes` live in `paramodel-elements` because
//! the trial *type* is the parameterisation of a specific element
//! graph over specific coordinates — the same semantic unit as the
//! element model.

pub mod error;
pub mod result;
pub mod status;
pub mod trial_set;

pub use error::{Result, TrialError};
pub use result::{
    ArtifactRef, ErrorInfo, ExecutionTiming, Metrics, TrialProvenance, TrialResult,
};
pub use status::TrialStatus;
pub use trial_set::{SamplingStrategy, TrialSet, TrialSetId};
