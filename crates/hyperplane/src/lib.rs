// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Hyperplane — a parameter-model-driven testing and orchestration
//! system.
//!
//! Hyperplane builds on [`paramodel`] — the algebraic core — and adds
//! hyperplane-specific elements, runtimes, and study-system glue. At
//! v0.1 only the paramodel foundation is published; this crate
//! re-exports [`paramodel`] under [`hyperplane::paramodel`] and stands
//! ready to re-export hyperplane-specific sub-crates as they land.

pub use paramodel;
