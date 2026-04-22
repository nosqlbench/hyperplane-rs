// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Compiler diagnostics and error type.
//!
//! Per SRD-0010: compilation emits a stream of `CompilationDiagnostic`
//! records. Any diagnostic with `Severity::Error` causes `compile` to
//! return `Err(CompilationError)` carrying every error collected in
//! the pass. Warnings flow through unless the caller escalates them
//! via `CompilerOptions::fail_on_warning_codes`.

use serde::{Deserialize, Serialize};

/// Diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Informational note.
    Info,
    /// Warning; plan still compiles unless escalated.
    Warn,
    /// Error; plan does not compile.
    Error,
}

/// Machine-readable diagnostic code.
///
/// Codes `W001`–`W099` are warnings (per SRD-0010 §Warnings catalogue);
/// codes `E001`–`E099` are plan-level errors. Code identity is stable
/// across compiler versions so tools can filter / escalate by code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum WarningCode {
    /// W001 — Exclusive dependent's target coalesces at a much
    /// broader scope than the dependent.
    W001,
    /// W002 — Two distinct element prototypes both exclusively
    /// depend on the same target within the same trial scope.
    W002,
    /// W003 — `EdgeFirst` trial ordering paired with a sampling
    /// strategy that may not include boundary points.
    W003,
    /// W004 — `max_concurrency > max_group_concurrency * group_count`.
    W004,
    /// W005 — Transitive reduction removed edges.
    W005,
    /// W006 — Coalesced LIFELINE cluster with more than two members.
    W006,

    /// E001 — Plan-level validation failed (wraps
    /// `paramodel_plan::PlanError`).
    E001,
    /// E002 — Unsupported feature in the v0.1 compiler.
    E002,
    /// E003 — Invariant violation (compiler bug).
    E003,
}

/// One diagnostic produced during compilation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilationDiagnostic {
    /// Severity.
    pub severity:   Severity,
    /// Stable code.
    pub code:       WarningCode,
    /// Human-readable message.
    pub message:    String,
    /// Where the diagnostic arose.
    pub location:   DiagnosticLocation,
    /// Suggested fix, if any.
    pub suggestion: Option<String>,
}

/// Pointer to the plan location a diagnostic concerns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiagnosticLocation {
    /// Whole plan.
    Plan,
    /// A specific element.
    Element {
        /// Element name.
        name: String,
    },
    /// A specific axis.
    Axis {
        /// Axis name.
        name: String,
    },
    /// A specific `(element, parameter)` coordinate.
    Parameter {
        /// Element name.
        element:   String,
        /// Parameter name.
        parameter: String,
    },
    /// A dependency edge.
    Dependency {
        /// Source element.
        source: String,
        /// Target element.
        target: String,
    },
}

// ---------------------------------------------------------------------------
// CompilationError.
// ---------------------------------------------------------------------------

/// Aggregate error returned by `Compiler::compile` when one or more
/// diagnostics at `Severity::Error` were produced.
#[derive(Debug, thiserror::Error)]
#[error("compilation failed with {} error(s)", .diagnostics.len())]
pub struct CompilationError {
    /// Every diagnostic collected — errors *and* warnings present at
    /// failure time. Callers iterate to report.
    pub diagnostics: Vec<CompilationDiagnostic>,
}

impl CompilationError {
    /// Construct from a single diagnostic.
    #[must_use]
    pub fn single(d: CompilationDiagnostic) -> Self {
        Self {
            diagnostics: vec![d],
        }
    }

    /// Construct from a list.
    #[must_use]
    pub const fn many(diagnostics: Vec<CompilationDiagnostic>) -> Self {
        Self { diagnostics }
    }
}

impl From<CompilationDiagnostic> for CompilationError {
    fn from(d: CompilationDiagnostic) -> Self {
        Self::single(d)
    }
}

/// Convenience helper: construct an error-severity diagnostic.
#[must_use]
pub fn error(
    code:     WarningCode,
    message:  impl Into<String>,
    location: DiagnosticLocation,
) -> CompilationDiagnostic {
    CompilationDiagnostic {
        severity: Severity::Error,
        code,
        message: message.into(),
        location,
        suggestion: None,
    }
}

/// Convenience helper: construct a warning-severity diagnostic.
#[must_use]
pub fn warn(
    code:     WarningCode,
    message:  impl Into<String>,
    location: DiagnosticLocation,
) -> CompilationDiagnostic {
    CompilationDiagnostic {
        severity: Severity::Warn,
        code,
        message: message.into(),
        location,
        suggestion: None,
    }
}

/// Convenience helper: construct an info-severity diagnostic.
#[must_use]
pub fn info(
    code:     WarningCode,
    message:  impl Into<String>,
    location: DiagnosticLocation,
) -> CompilationDiagnostic {
    CompilationDiagnostic {
        severity: Severity::Info,
        code,
        message: message.into(),
        location,
        suggestion: None,
    }
}
