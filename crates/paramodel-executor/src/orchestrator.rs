// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `Orchestrator` trait + `DefaultOrchestrator`.
//!
//! The orchestrator translates an `AtomicStep` into a call on a
//! concrete `ElementRuntime`. V0.1 takes a pre-resolved
//! `BTreeMap<ElementName, Arc<dyn ElementRuntime>>` rather than an
//! `ElementRuntimeRegistry`: `ExecutionPlan` doesn't re-carry the
//! authored `Element` records a registry would need to dispatch
//! against. Registry-based resolution arrives when the compiler
//! starts attaching element records to the plan's execution metadata.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use paramodel_elements::{
    ElementName, ElementRuntime, HealthCheckSpec, LiveStatusSummary, MaterializationOutputs,
    ParameterName, Trial, Value,
};
use paramodel_plan::{AtomicStep, ExecutionPlan, InstanceId, ShutdownReason};
use paramodel_trials::{ArtifactRef, ErrorInfo};
use serde::{Deserialize, Serialize};

use crate::error::OrchestratorError;

/// Outcome of one `execute_step` call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StepOutcome {
    /// Step finished cleanly.
    Completed {
        /// Wall-clock elapsed.
        duration: Duration,
        /// Typed outputs from a `Deploy` step.
        #[serde(default)]
        materialization_outputs: MaterializationOutputs,
        /// Metrics captured by a `SaveOutput` step.
        #[serde(default)]
        metrics: BTreeMap<ParameterName, Value>,
        /// Artifacts captured by a `SaveOutput` / `Teardown`.
        #[serde(default)]
        artifacts: Vec<ArtifactRef>,
    },
    /// Step was skipped.
    Skipped {
        /// Reason.
        reason: String,
    },
    /// Step failed.
    Failed {
        /// Error payload.
        error: ErrorInfo,
    },
}

// ---------------------------------------------------------------------------
// Orchestrator trait.
// ---------------------------------------------------------------------------

/// System runtime that bridges the graph to `ElementRuntime`s.
#[async_trait]
pub trait Orchestrator: Send + Sync + 'static {
    /// Prepare for a run — register every element, resolve runtimes,
    /// acquire long-lived resources.
    async fn prepare(&self, plan: &ExecutionPlan) -> Result<(), OrchestratorError>;

    /// Execute one step against the correct `ElementRuntime`.
    async fn execute_step(
        &self,
        plan: &ExecutionPlan,
        step: &AtomicStep,
    ) -> Result<StepOutcome, OrchestratorError>;

    /// Await an instance's readiness per its health-check spec.
    async fn await_ready(
        &self,
        instance_id: &InstanceId,
        spec:        &HealthCheckSpec,
    ) -> Result<(), OrchestratorError>;

    /// Deliver `on_trial_starting` to every listed non-trial element.
    async fn notify_trial_start(
        &self,
        trial:    &Trial,
        elements: &[ElementName],
    ) -> Result<(), OrchestratorError>;

    /// Deliver `on_trial_ending` to every listed non-trial element.
    async fn notify_trial_end(
        &self,
        trial:    &Trial,
        elements: &[ElementName],
        reason:   ShutdownReason,
    ) -> Result<(), OrchestratorError>;

    /// Final teardown of any live instances + resource release.
    async fn shutdown(&self) -> Result<(), OrchestratorError>;

    /// Query operational state of a live instance.
    async fn instance_state(&self, instance_id: &InstanceId) -> Option<LiveStatusSummary>;
}

// ---------------------------------------------------------------------------
// DefaultOrchestrator.
// ---------------------------------------------------------------------------

/// Reference orchestrator backed by a pre-resolved runtime map.
pub struct DefaultOrchestrator {
    runtimes: Mutex<BTreeMap<ElementName, Arc<dyn ElementRuntime>>>,
}

impl std::fmt::Debug for DefaultOrchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.runtimes.lock().expect("poisoned");
        f.debug_struct("DefaultOrchestrator")
            .field("runtimes", &guard.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl DefaultOrchestrator {
    /// Construct over a pre-resolved runtime map.
    #[must_use]
    pub fn new(runtimes: BTreeMap<ElementName, Arc<dyn ElementRuntime>>) -> Self {
        Self {
            runtimes: Mutex::new(runtimes),
        }
    }

    /// Empty orchestrator (for plans with no element steps).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            runtimes: Mutex::new(BTreeMap::new()),
        }
    }

    fn runtime_for(
        &self,
        element: &ElementName,
    ) -> Result<Arc<dyn ElementRuntime>, OrchestratorError> {
        self.runtimes
            .lock()
            .expect("poisoned")
            .get(element)
            .cloned()
            .ok_or_else(|| OrchestratorError::NoRuntimeForElement {
                element: element.as_str().to_owned(),
            })
    }
}

#[async_trait]
impl Orchestrator for DefaultOrchestrator {
    async fn prepare(&self, _plan: &ExecutionPlan) -> Result<(), OrchestratorError> {
        Ok(())
    }

    async fn execute_step(
        &self,
        _plan: &ExecutionPlan,
        step:  &AtomicStep,
    ) -> Result<StepOutcome, OrchestratorError> {
        match step {
            AtomicStep::Deploy { element, configuration, .. } => {
                let rt = self.runtime_for(element)?;
                let outputs = rt.materialize(configuration).await.map_err(|err| {
                    OrchestratorError::UnsupportedStep {
                        step:   step.id().as_str().to_owned(),
                        reason: format!("materialize failed: {err}"),
                    }
                })?;
                Ok(StepOutcome::Completed {
                    duration:                Duration::ZERO,
                    materialization_outputs: outputs,
                    metrics:                 BTreeMap::new(),
                    artifacts:               Vec::new(),
                })
            }
            AtomicStep::Teardown { element, .. } => {
                let rt = self.runtime_for(element)?;
                rt.dematerialize().await.map_err(|err| OrchestratorError::UnsupportedStep {
                    step:   step.id().as_str().to_owned(),
                    reason: format!("dematerialize failed: {err}"),
                })?;
                Ok(StepOutcome::Completed {
                    duration:                Duration::ZERO,
                    materialization_outputs: MaterializationOutputs::new(),
                    metrics:                 BTreeMap::new(),
                    artifacts:               Vec::new(),
                })
            }
            AtomicStep::Checkpoint { .. } | AtomicStep::Barrier { .. } => Ok(
                StepOutcome::Completed {
                    duration:                Duration::ZERO,
                    materialization_outputs: MaterializationOutputs::new(),
                    metrics:                 BTreeMap::new(),
                    artifacts:               Vec::new(),
                },
            ),
            other => Err(OrchestratorError::UnsupportedStep {
                step:   other.id().as_str().to_owned(),
                reason: "v0.1 orchestrator handles Deploy, Teardown, Checkpoint, Barrier only"
                    .to_owned(),
            }),
        }
    }

    async fn await_ready(
        &self,
        _instance_id: &InstanceId,
        _spec:        &HealthCheckSpec,
    ) -> Result<(), OrchestratorError> {
        Ok(())
    }

    async fn notify_trial_start(
        &self,
        _trial:    &Trial,
        _elements: &[ElementName],
    ) -> Result<(), OrchestratorError> {
        Ok(())
    }

    async fn notify_trial_end(
        &self,
        _trial:    &Trial,
        _elements: &[ElementName],
        _reason:   ShutdownReason,
    ) -> Result<(), OrchestratorError> {
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), OrchestratorError> {
        self.runtimes.lock().expect("poisoned").clear();
        Ok(())
    }

    async fn instance_state(&self, _instance_id: &InstanceId) -> Option<LiveStatusSummary> {
        None
    }
}
