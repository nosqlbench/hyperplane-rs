// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `AtomicStep` — the indivisible unit of work the executor runs.
//!
//! Per SRD-0009 D1 / D2 there are eight variants, each prefixed with
//! a common [`StepHeader`] carrying id, dependencies, reason, trial
//! context, timing, resources, retry policy, and metadata. Variant
//! accessors (`element`, `trial_id`) return `None` for variants that
//! don't apply so callers don't have to pattern-match everywhere.

use std::time::Duration;

use paramodel_elements::{ElementName, Labels, ParameterName, ResolvedConfiguration, Tags, TrialId};
use serde::{Deserialize, Serialize};

use crate::error::PlanError;
use crate::policies::RetryPolicy;

// ---------------------------------------------------------------------------
// StepId + BarrierId + CheckpointId — validated string newtypes.
// ---------------------------------------------------------------------------

const STEP_ID_MAX_LEN: usize = 256;

fn validate_identifier(s: &str, max: usize) -> std::result::Result<(), PlanError> {
    if s.is_empty() {
        return Err(PlanError::InvalidStepId {
            reason: "must not be empty".to_owned(),
        });
    }
    if s.len() > max {
        return Err(PlanError::InvalidStepId {
            reason: format!("must be ≤ {max} bytes (got {})", s.len()),
        });
    }
    for (offset, ch) in s.char_indices() {
        if !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':')) {
            return Err(PlanError::InvalidStepId {
                reason: format!("invalid character '{ch}' at byte offset {offset}"),
            });
        }
    }
    Ok(())
}

macro_rules! id_newtype {
    (
        $(#[$meta:meta])*
        $vis:vis struct $Name:ident
    ) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        $vis struct $Name(String);

        impl $Name {
            #[doc = "Construct, validating the candidate string."]
            pub fn new(s: impl Into<String>) -> std::result::Result<Self, PlanError> {
                let s = s.into();
                validate_identifier(&s, STEP_ID_MAX_LEN)?;
                Ok(Self(s))
            }

            #[doc = "Borrow the inner string."]
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[doc = "Consume and return the inner string."]
            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $Name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $Name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl std::fmt::Debug for $Name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, concat!(stringify!($Name), "({:?})"), self.0)
            }
        }

        impl Serialize for $Name {
            fn serialize<S: serde::Serializer>(
                &self,
                s: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $Name {
            fn deserialize<D: serde::Deserializer<'de>>(
                d: D,
            ) -> std::result::Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Self::new(s).map_err(serde::de::Error::custom)
            }
        }
    };
}

id_newtype! {
    /// Human-readable, deterministic step identifier.
    ///
    /// Reducto's naming convention — `activate_{element}_t{index}`,
    /// `deactivate_{element}_g{group}`, `barrier_{id}`, and so on —
    /// makes plans diffable across compilations.
    pub struct StepId
}

id_newtype! {
    /// Identifier for a barrier step payload.
    pub struct BarrierId
}

id_newtype! {
    /// Identifier for a checkpoint step payload.
    pub struct CheckpointId
}

// ---------------------------------------------------------------------------
// Helper enums embedded in AtomicStep variants.
// ---------------------------------------------------------------------------

/// Why a trial was shut down — attached to `AtomicStep::TrialEnd`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownReason {
    /// Trial completed as expected.
    Normal,
    /// User-initiated cancel.
    Cancelled,
    /// Exceeded `trial_timeout`.
    Timeout,
    /// A dependency failed.
    UpstreamFailed,
    /// Planner-emitted (e.g. partial-run boundary).
    Planned,
}

/// Kinds of plan-level barrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarrierKind {
    /// Wait until all dependencies have activated an element.
    ElementReady,
    /// Wait until all trials using an element have completed.
    ElementScopeEnd,
    /// Group trials for a checkpoint boundary.
    TrialBatch,
    /// Force state persistence before proceeding.
    CheckpointBoundary,
    /// Adopter-defined synchronisation point.
    Custom,
}

/// What to do when a barrier times out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutAction {
    /// Fail immediately when the barrier times out.
    FailFast,
    /// Skip any dependent steps and proceed.
    SkipDependent,
    /// Wait indefinitely (no timeout enforcement).
    WaitForever,
    /// Retry the wait per the step's `RetryPolicy`.
    Retry,
}

/// What a `SaveOutput` step captures.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputSelector {
    /// A specific declared result parameter — the value is placed in
    /// the trial's metrics under `(element, result_parameter)`.
    ResultParameter {
        /// The declared `result_parameters` name.
        parameter: ParameterName,
    },
    /// Whole output-volume capture. The result is placed in the
    /// trial's `artifacts` list.
    Volume {
        /// Mount point whose contents get archived.
        mount: String,
    },
}

// ---------------------------------------------------------------------------
// StepHeader.
// ---------------------------------------------------------------------------

/// Common prefix attached to every `AtomicStep` variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, bon::Builder)]
pub struct StepHeader {
    /// The step's identifier — unique within its
    /// [`crate::ExecutionGraph`].
    pub id: StepId,

    /// Identifiers of steps that must complete before this one runs.
    #[builder(default)]
    pub depends_on: Vec<StepId>,

    /// Human-readable explanation of why this step exists, matching
    /// reducto's `reason` annotation.
    pub reason: String,

    /// Trial index this step belongs to. `None` for steps outside
    /// any trial (study-scope deploys, start/end sentinels).
    pub trial_index: Option<u32>,

    /// Reducto mixed-radix trial code (e.g. `"0x0003"`). `None` if
    /// the step isn't trial-scoped.
    pub trial_code: Option<String>,

    /// Compiler-estimated wall-clock duration.
    pub estimated_duration: Option<Duration>,

    /// Resource requirements for this one step.
    #[builder(default)]
    pub resource_requirements: crate::ResourceRequirements,

    /// Step-level retry-policy override; `None` falls back to the
    /// plan-level policy.
    pub retry_policy: Option<RetryPolicy>,

    /// Intrinsic facts.
    #[builder(default)]
    pub labels: Labels,

    /// Organisational tags.
    #[builder(default)]
    pub tags: Tags,
}

// ---------------------------------------------------------------------------
// AtomicStep — eight variants per SRD-0009 D1.
// ---------------------------------------------------------------------------

/// The indivisible unit of work in an `ExecutionGraph`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AtomicStep {
    /// Provision an element instance with a resolved configuration.
    Deploy {
        /// Common step metadata.
        header: StepHeader,
        /// The element being deployed.
        element: ElementName,
        /// This deployment's instance number for the element.
        instance_number: u32,
        /// Fully-interpolated configuration (tokens expanded).
        configuration: ResolvedConfiguration,
        /// Per-step concurrency cap (SRD-0002 R26).
        max_concurrency: Option<u32>,
        /// Per-group concurrency cap (SRD-0002 R26, Rule 6). Executor
        /// observes this as a declarative directive; the compiler does
        /// not encode it as structural edges.
        #[serde(default)]
        max_group_concurrency: Option<u32>,
        /// Owner element when this Deploy targets a dedicated instance
        /// (Rule 2 Dedicated). `None` for standalone deployments.
        #[serde(default)]
        dedicated_to: Option<ElementName>,
    },

    /// Release an element instance's provisioned resources.
    Teardown {
        /// Common step metadata.
        header: StepHeader,
        /// The element being torn down.
        element: ElementName,
        /// Which instance.
        instance_number: u32,
        /// Whether to capture outputs before teardown.
        collect_artifacts: bool,
    },

    /// Notify non-trial elements that a trial is about to begin.
    TrialStart {
        /// Common step metadata.
        header: StepHeader,
        /// Starting trial's id.
        trial_id: TrialId,
        /// Non-trial elements to notify.
        element_names: Vec<ElementName>,
    },

    /// Notify non-trial elements that a trial has concluded.
    TrialEnd {
        /// Common step metadata.
        header: StepHeader,
        /// Ending trial's id.
        trial_id: TrialId,
        /// Non-trial elements to notify.
        element_names: Vec<ElementName>,
        /// Why the trial ended.
        shutdown_reason: ShutdownReason,
    },

    /// Wait for a command-mode element instance to self-terminate.
    Await {
        /// Common step metadata.
        header: StepHeader,
        /// The element being awaited.
        element: ElementName,
        /// Which instance.
        instance_number: u32,
        /// Trial this await belongs to.
        trial_id: TrialId,
    },

    /// Fetch typed output from a completed element instance.
    SaveOutput {
        /// Common step metadata.
        header: StepHeader,
        /// The element whose output is being captured.
        element: ElementName,
        /// Which instance.
        instance_number: u32,
        /// Trial this capture belongs to.
        trial_id: TrialId,
        /// What exactly to capture.
        output: OutputSelector,
    },

    /// Plan-layer synchronisation point. Runtime state machinery
    /// (wait / release / fail) is in the executor SRD.
    Barrier {
        /// Common step metadata.
        header: StepHeader,
        /// Payload identifier.
        barrier_id: BarrierId,
        /// Barrier kind. Named `barrier_kind` (not `kind`) so it
        /// doesn't collide with the outer `#[serde(tag = "kind")]`.
        barrier_kind: BarrierKind,
        /// Optional wait cap.
        timeout: Option<Duration>,
        /// What to do when `timeout` expires.
        timeout_action: TimeoutAction,
    },

    /// Persist execution state mid-run for resume.
    Checkpoint {
        /// Common step metadata.
        header: StepHeader,
        /// Payload identifier.
        checkpoint_id: CheckpointId,
    },
}

impl AtomicStep {
    /// Common metadata prefix.
    #[must_use]
    pub const fn header(&self) -> &StepHeader {
        match self {
            Self::Deploy { header, .. }
            | Self::Teardown { header, .. }
            | Self::TrialStart { header, .. }
            | Self::TrialEnd { header, .. }
            | Self::Await { header, .. }
            | Self::SaveOutput { header, .. }
            | Self::Barrier { header, .. }
            | Self::Checkpoint { header, .. } => header,
        }
    }

    /// Step id.
    #[must_use]
    pub const fn id(&self) -> &StepId {
        &self.header().id
    }

    /// Dependency ids this step waits on.
    #[must_use]
    pub fn depends_on(&self) -> &[StepId] {
        &self.header().depends_on
    }

    /// Element the step targets, if applicable.
    #[must_use]
    pub const fn element(&self) -> Option<&ElementName> {
        match self {
            Self::Deploy { element, .. }
            | Self::Teardown { element, .. }
            | Self::Await { element, .. }
            | Self::SaveOutput { element, .. } => Some(element),
            _ => None,
        }
    }

    /// Instance number, if applicable.
    #[must_use]
    pub const fn instance_number(&self) -> Option<u32> {
        match self {
            Self::Deploy { instance_number, .. }
            | Self::Teardown { instance_number, .. }
            | Self::Await { instance_number, .. }
            | Self::SaveOutput { instance_number, .. } => Some(*instance_number),
            _ => None,
        }
    }

    /// Trial id, if applicable.
    #[must_use]
    pub const fn trial_id(&self) -> Option<&TrialId> {
        match self {
            Self::TrialStart { trial_id, .. }
            | Self::TrialEnd { trial_id, .. }
            | Self::Await { trial_id, .. }
            | Self::SaveOutput { trial_id, .. } => Some(trial_id),
            _ => None,
        }
    }

    /// `true` if this step is `AtomicStep::Barrier`.
    #[must_use]
    pub const fn is_barrier(&self) -> bool {
        matches!(self, Self::Barrier { .. })
    }

    /// `true` if this step is `AtomicStep::Checkpoint`.
    #[must_use]
    pub const fn is_checkpoint(&self) -> bool {
        matches!(self, Self::Checkpoint { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ulid::Ulid;

    fn sid(s: &str) -> StepId {
        StepId::new(s).unwrap()
    }
    fn tid(n: u64) -> TrialId {
        TrialId::from_ulid(Ulid::from_parts(n, 1))
    }

    fn header(id: &str) -> StepHeader {
        StepHeader::builder()
            .id(sid(id))
            .reason("initial deploy".to_owned())
            .build()
    }

    #[test]
    fn step_id_validates_and_rejects_bad_inputs() {
        StepId::new("activate_jvector_t3").unwrap();
        StepId::new("barrier:cp-1").unwrap();
        assert!(StepId::new("").is_err());
        assert!(StepId::new("has space").is_err());
        let overlong = "a".repeat(STEP_ID_MAX_LEN + 1);
        assert!(StepId::new(overlong).is_err());
    }

    #[test]
    fn step_id_serde_roundtrip() {
        let id = sid("activate_x_t1");
        let json = serde_json::to_string(&id).unwrap();
        let back: StepId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn step_header_builder_defaults_fields() {
        let h = header("step_1");
        assert!(h.depends_on.is_empty());
        assert!(h.retry_policy.is_none());
        assert!(h.labels.is_empty());
    }

    #[test]
    fn atomic_step_header_dispatch() {
        let s = AtomicStep::Deploy {
            header:          header("deploy_1"),
            element:         ElementName::new("db").unwrap(),
            instance_number: 0,
            configuration:   ResolvedConfiguration::new(),
            max_concurrency: None,
            max_group_concurrency: None,
            dedicated_to: None,
        };
        assert_eq!(s.id().as_str(), "deploy_1");
        assert_eq!(s.element().map(ElementName::as_str), Some("db"));
        assert_eq!(s.instance_number(), Some(0));
        assert!(!s.is_barrier());
    }

    #[test]
    fn atomic_step_trial_id_accessor() {
        let s = AtomicStep::TrialStart {
            header:        header("notify_trial_start_t0"),
            trial_id:      tid(1),
            element_names: vec![ElementName::new("db").unwrap()],
        };
        assert!(s.trial_id().is_some());
        assert!(s.element().is_none());
    }

    #[test]
    fn atomic_step_serde_roundtrip() {
        let s = AtomicStep::Barrier {
            header:         header("barrier_cp1"),
            barrier_id:     BarrierId::new("cp1").unwrap(),
            barrier_kind:   BarrierKind::CheckpointBoundary,
            #[allow(
                clippy::duration_suboptimal_units,
                reason = "seconds match the SRD's timeout vocabulary"
            )]
            timeout:        Some(Duration::from_secs(60)),
            timeout_action: TimeoutAction::FailFast,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: AtomicStep = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn output_selector_variants_serde() {
        let rp = OutputSelector::ResultParameter {
            parameter: ParameterName::new("p50_latency").unwrap(),
        };
        let vol = OutputSelector::Volume {
            mount: "/out".to_owned(),
        };
        for x in [rp, vol] {
            let json = serde_json::to_string(&x).unwrap();
            let back: OutputSelector = serde_json::from_str(&json).unwrap();
            assert_eq!(x, back);
        }
    }
}
