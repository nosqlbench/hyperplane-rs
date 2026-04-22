// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `TrialStatus` — the trial lifecycle enum.

use serde::{Deserialize, Serialize};

/// Trial lifecycle state.
///
/// `Pending` and `InProgress` are non-terminal; everything else is
/// terminal from the scheduler's point of view.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TrialStatus {
    /// Queued, not yet picked up.
    Pending,
    /// Currently executing.
    InProgress,
    /// Executed successfully.
    Completed,
    /// Executed but failed.
    Failed,
    /// Not executed — upstream failure, precondition not met, etc.
    Skipped,
    /// Cancelled by the user.
    Cancelled,
}

impl TrialStatus {
    /// `true` for every non-running state.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        !matches!(self, Self::Pending | Self::InProgress)
    }

    /// `true` only for `Completed`.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self, Self::Completed)
    }

    /// `true` only for `Failed`.
    #[must_use]
    pub const fn is_failure(&self) -> bool {
        matches!(self, Self::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_predicate() {
        assert!(!TrialStatus::Pending.is_terminal());
        assert!(!TrialStatus::InProgress.is_terminal());
        assert!(TrialStatus::Completed.is_terminal());
        assert!(TrialStatus::Failed.is_terminal());
        assert!(TrialStatus::Skipped.is_terminal());
        assert!(TrialStatus::Cancelled.is_terminal());
    }

    #[test]
    fn success_and_failure_predicates() {
        assert!(TrialStatus::Completed.is_success());
        assert!(!TrialStatus::Failed.is_success());
        assert!(TrialStatus::Failed.is_failure());
        assert!(!TrialStatus::Completed.is_failure());
    }

    #[test]
    fn serde_is_snake_case() {
        let s = serde_json::to_string(&TrialStatus::InProgress).unwrap();
        assert_eq!(s, "\"in_progress\"");
        let back: TrialStatus = serde_json::from_str(&s).unwrap();
        assert_eq!(back, TrialStatus::InProgress);
    }
}
