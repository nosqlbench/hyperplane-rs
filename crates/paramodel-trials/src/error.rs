// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Crate-local error type for the trial-set / trial-result layer.

/// Errors produced by `TrialSet`, `SamplingStrategy`, and
/// `TrialResult` construction / validation.
///
/// Trial-data errors (assignments, mis-addressed values) live on
/// `paramodel_elements::TrialError` since `Trial` itself lives there.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TrialError {
    /// A `SamplingStrategy::Random` / `Linspace` constructor received
    /// `count == 0`.
    #[error("sampling strategy count must be >= 1")]
    SamplingZeroCount,

    /// `TrialResult.attempt` was zero — attempt numbers start at 1.
    #[error("trial result attempt must be >= 1")]
    ZeroAttempt,

    /// `status == Failed` but no `error` payload was attached.
    #[error("trial result has status Failed but error field is empty")]
    MissingErrorForFailure,

    /// `status == Skipped` but no `skip_reason` was attached.
    #[error("trial result has status Skipped but skip_reason is empty")]
    MissingSkipReason,

    /// `error` attached to a non-Failed status.
    #[error("trial result carries error but status is not Failed")]
    ErrorWithoutFailure,

    /// `skip_reason` attached to a non-Skipped status.
    #[error("trial result carries skip_reason but status is not Skipped")]
    SkipReasonWithoutSkip,

    /// `completed_at < started_at` in an `ExecutionTiming`.
    #[error("execution timing completed_at precedes started_at")]
    NegativeDuration,
}

/// Crate-local `Result` alias.
pub type Result<T, E = TrialError> = std::result::Result<T, E>;
