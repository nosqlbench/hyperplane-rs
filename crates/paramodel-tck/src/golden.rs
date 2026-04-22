// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Golden-file harness for compiled `ExecutionGraph` shape.
//!
//! Each golden captures the stable structure of a compiled plan —
//! step ids, variants, per-step dependency sets — so regressions in
//! compiler behaviour surface as diffs the human can read. Heavy
//! metadata (timestamps, ULIDs) is excluded from the capture.
//!
//! Regenerate goldens by setting `PARAMODEL_TCK_BLESS=1` before
//! running `cargo test`.

use std::path::Path;

use paramodel_plan::{AtomicStep, ExecutionPlan};
use serde::{Deserialize, Serialize};

/// Per-step skeleton the golden records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepSkeleton {
    /// Step id.
    pub id:          String,
    /// Variant name (`"deploy"`, `"teardown"`, `"trial_start"`, …).
    pub kind:        String,
    /// Dependencies in sorted order.
    pub depends_on:  Vec<String>,
    /// Element this step targets, if applicable.
    pub element:     Option<String>,
    /// Trial index, if applicable.
    pub trial_index: Option<u32>,
}

/// Whole-plan golden — an ordered list of step skeletons (sorted by
/// id) plus the raw step count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSkeleton {
    /// All steps, sorted by id.
    pub steps:      Vec<StepSkeleton>,
    /// Fast-access step count for sanity.
    pub step_count: usize,
}

impl PlanSkeleton {
    /// Build a skeleton from a compiled plan.
    #[must_use]
    pub fn from_plan(plan: &ExecutionPlan) -> Self {
        let mut steps: Vec<StepSkeleton> = plan
            .steps()
            .iter()
            .map(Self::skeleton_of)
            .collect();
        steps.sort_by(|a, b| a.id.cmp(&b.id));
        let step_count = steps.len();
        Self { steps, step_count }
    }

    fn skeleton_of(step: &AtomicStep) -> StepSkeleton {
        let kind: String = match step {
            AtomicStep::Deploy { .. } => "deploy",
            AtomicStep::Teardown { .. } => "teardown",
            AtomicStep::TrialStart { .. } => "trial_start",
            AtomicStep::TrialEnd { .. } => "trial_end",
            AtomicStep::Await { .. } => "await",
            AtomicStep::SaveOutput { .. } => "save_output",
            AtomicStep::Barrier { .. } => "barrier",
            AtomicStep::Checkpoint { .. } => "checkpoint",
        }
        .to_owned();
        let mut depends_on: Vec<String> = step
            .depends_on()
            .iter()
            .map(|d| d.as_str().to_owned())
            .collect();
        depends_on.sort();
        StepSkeleton {
            id: step.id().as_str().to_owned(),
            kind,
            depends_on,
            element: step.element().map(|e| e.as_str().to_owned()),
            trial_index: step.header().trial_index,
        }
    }
}

/// Compare the supplied plan against the golden file at `path`.
///
/// When the environment variable `PARAMODEL_TCK_BLESS` is set and
/// non-empty, the golden is (re)written from the current plan and
/// the assertion becomes trivially true. Otherwise the golden is
/// loaded and compared; a diff-style assertion failure is raised on
/// mismatch.
///
/// Returns `Err(String)` if the golden file is missing (or invalid)
/// and we're not blessing; propagation is left to the caller so that
/// the test can use `expect` / `assert` patterns that integrate with
/// its harness.
pub fn assert_golden(plan: &ExecutionPlan, path: impl AsRef<Path>) -> Result<(), String> {
    let path = path.as_ref();
    let skeleton = PlanSkeleton::from_plan(plan);
    let current =
        serde_json::to_string_pretty(&skeleton).map_err(|e| format!("serialize: {e}"))?;

    if std::env::var("PARAMODEL_TCK_BLESS")
        .ok()
        .is_some_and(|v| !v.is_empty())
    {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        std::fs::write(path, format!("{current}\n"))
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        return Ok(());
    }

    let expected = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    let expected = expected.trim_end();
    let current = current.trim_end();
    if expected == current {
        return Ok(());
    }
    Err(format!(
        "golden mismatch at {path}\n---expected---\n{expected}\n---actual---\n{current}\n",
        path = path.display(),
    ))
}
