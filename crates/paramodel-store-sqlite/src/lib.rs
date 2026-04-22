// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Paramodel persistence — `SQLite` backend (placeholder).
//!
//! This crate reserves the `paramodel-store-sqlite` namespace on
//! crates.io. v0.1 ships no concrete implementations — schema,
//! migrations, and trait impls land in follow-up slices. When they
//! do, each of the six [`paramodel_persistence`] traits will be
//! implemented against a `SQLite` backend behind `SqliteStore`.

// Re-export the persistence-trait surface so adopters only need
// `paramodel-store-sqlite` as a single dependency once the backend
// is implemented.
pub use paramodel_persistence as persistence;
