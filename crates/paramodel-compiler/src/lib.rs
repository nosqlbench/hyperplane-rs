// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Paramodel compiler.
//!
//! v0.1 scope covers the trivial case end-to-end; the full reducto
//! pipeline (SRD-0010) is filled in across follow-up slices. See the
//! `compiler` module for the feature envelope.

pub mod binding;
pub mod compiler;
pub mod enumerator;
pub mod error;
pub mod options;
pub mod trial_element;

pub use binding::BindingStateComputer;
pub use compiler::{Compiler, DefaultCompiler};
pub use enumerator::MixedRadixEnumerator;
pub use error::{
    CompilationDiagnostic, CompilationError, DiagnosticLocation, Severity, WarningCode,
};
pub use options::{CompilerOptions, CustomOrderingRegistry, EmptyCustomOrderingRegistry};
pub use trial_element::identify_trial_elements;
