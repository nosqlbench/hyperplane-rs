// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Element configuration and exports.
//!
//! `Configuration` holds the element's authored parameter bindings —
//! literal values or [`TokenExpr`] references. It is *not* a defaults
//! map: per SRD-0007 D21 the resolver precedence for a parameter's
//! trial value is `axis binding → element configuration → parameter
//! default → error`.
//!
//! `Exports` maps user-defined export names (e.g. `service_addr`) to
//! token expressions (`${self.ip}:4567`) that downstream elements can
//! reference. Resolution happens at deploy time.

use std::collections::BTreeMap;

use crate::{ParameterName, Value, name_type};
use serde::{Deserialize, Serialize};

use crate::error::ElementError;

// ---------------------------------------------------------------------------
// TokenExpr — opaque at this layer.
// ---------------------------------------------------------------------------

/// A token-expression reference.
///
/// Opaque at the element layer: the grammar (`${self.ip}`,
/// `${other_element.endpoint}`, `${{db:dockerRegistry}}`, …) lives in
/// the test-plan / compilation SRDs, which own parsing and resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TokenExpr(String);

impl TokenExpr {
    /// Construct a token expression. Rejects empty strings.
    pub fn new(s: impl Into<String>) -> Result<Self, ElementError> {
        let s = s.into();
        if s.is_empty() {
            return Err(ElementError::EmptyTokenExpr);
        }
        Ok(Self(s))
    }

    /// Borrow the raw expression source.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume and return the raw source.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }

    /// If this expression is a bare `${KEY}` (no surrounding text,
    /// no nested `$` or braces), return `KEY`. Otherwise `None`.
    ///
    /// The compiler uses this to classify simple single-token
    /// expressions that can be handed verbatim to a
    /// [`TokenResolver`]. Richer grammars (string interpolation,
    /// arithmetic, conditionals) are deferred to a follow-up slice
    /// and will parse the full string separately.
    #[must_use]
    pub fn as_single_key(&self) -> Option<&str> {
        let inner = self.0.strip_prefix("${")?.strip_suffix('}')?;
        if inner.is_empty() || inner.contains('$')
            || inner.contains('{') || inner.contains('}')
        {
            return None;
        }
        Some(inner)
    }
}

impl std::fmt::Display for TokenExpr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for TokenExpr {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TokenExpr {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// TokenResolver — host-provided resolution hook.
// ---------------------------------------------------------------------------

/// Host-provided resolver that maps a bare token key (as extracted
/// by [`TokenExpr::as_single_key`]) to a typed [`Value`].
///
/// The compiler calls `resolve(key, target_parameter)` during
/// configuration resolution. Returning `Some(value)` binds the value
/// to the target parameter as if it were a `Literal` at the same
/// precedence level as the original `Token`. Returning `None` lets
/// the compiler fall through to the next precedence level (plan
/// binding → element default → error).
pub trait TokenResolver: Send + Sync + std::fmt::Debug + 'static {
    /// Resolve a bare key for the given target parameter.
    fn resolve(&self, key: &str, target: &ParameterName) -> Option<Value>;
}

// ---------------------------------------------------------------------------
// ConfigEntry and Configuration.
// ---------------------------------------------------------------------------

/// One configuration slot on an element.
///
/// `Literal` pins an immediate value; `Token` refers to a runtime-
/// resolved expression. The compiler replaces `Token` entries with
/// their resolved `Value`s before execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigEntry {
    /// A literal, typed value bound to the parameter.
    Literal {
        /// The pinned value.
        value: Value,
    },
    /// A token expression resolved at plan-compile time.
    Token {
        /// The token-expression source.
        expr: TokenExpr,
    },
}

impl ConfigEntry {
    /// Wrap a value as a literal entry.
    #[must_use]
    pub const fn literal(value: Value) -> Self {
        Self::Literal { value }
    }

    /// Wrap a token expression as a token entry.
    #[must_use]
    pub const fn token(expr: TokenExpr) -> Self {
        Self::Token { expr }
    }

    /// Does this entry need token resolution before use?
    #[must_use]
    pub const fn is_token(&self) -> bool {
        matches!(self, Self::Token { .. })
    }
}

/// The element's authored parameter bindings. Not a defaults map —
/// see SRD-0007 D21.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Configuration(BTreeMap<ParameterName, ConfigEntry>);

impl Configuration {
    /// Empty configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace an entry. Returns the previous entry for
    /// `name`, if any.
    pub fn insert(
        &mut self,
        name:  ParameterName,
        entry: ConfigEntry,
    ) -> Option<ConfigEntry> {
        self.0.insert(name, entry)
    }

    /// Look up an entry.
    #[must_use]
    pub fn get(&self, name: &ParameterName) -> Option<&ConfigEntry> {
        self.0.get(name)
    }

    /// Sorted-by-key iterator over `(name, entry)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&ParameterName, &ConfigEntry)> {
        self.0.iter()
    }

    /// Sorted key iterator.
    pub fn keys(&self) -> impl Iterator<Item = &ParameterName> {
        self.0.keys()
    }

    /// Entry count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<(ParameterName, ConfigEntry)> for Configuration {
    fn from_iter<I: IntoIterator<Item = (ParameterName, ConfigEntry)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

// ---------------------------------------------------------------------------
// ExportName + Exports.
// ---------------------------------------------------------------------------

name_type! {
    /// Name of one exported value an element publishes. Identifier-style
    /// (ASCII alphanumeric + `_-.`).
    pub struct ExportName { kind: "ExportName" }
}

/// Map from export name to token expression.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Exports(BTreeMap<ExportName, TokenExpr>);

impl Exports {
    /// Empty export map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace an export. Returns the previous expression
    /// for `name`, if any.
    pub fn insert(&mut self, name: ExportName, expr: TokenExpr) -> Option<TokenExpr> {
        self.0.insert(name, expr)
    }

    /// Look up an export.
    #[must_use]
    pub fn get(&self, name: &ExportName) -> Option<&TokenExpr> {
        self.0.get(name)
    }

    /// Sorted-by-key iterator.
    pub fn iter(&self) -> impl Iterator<Item = (&ExportName, &TokenExpr)> {
        self.0.iter()
    }

    /// Sorted key iterator.
    pub fn keys(&self) -> impl Iterator<Item = &ExportName> {
        self.0.keys()
    }

    /// Export count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<(ExportName, TokenExpr)> for Exports {
    fn from_iter<I: IntoIterator<Item = (ExportName, TokenExpr)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ParameterName;

    fn pname(s: &str) -> ParameterName {
        ParameterName::new(s).unwrap()
    }

    #[test]
    fn token_expr_rejects_empty() {
        assert!(TokenExpr::new("").is_err());
        let t = TokenExpr::new("${self.ip}").unwrap();
        assert_eq!(t.as_str(), "${self.ip}");
    }

    #[test]
    fn config_entry_helpers() {
        let lit = ConfigEntry::literal(Value::integer(pname("n"), 8, None));
        let tok = ConfigEntry::token(TokenExpr::new("${self.ip}").unwrap());
        assert!(!lit.is_token());
        assert!(tok.is_token());
    }

    #[test]
    fn configuration_iter_is_sorted_by_name() {
        let mut c = Configuration::new();
        c.insert(
            pname("zebra"),
            ConfigEntry::literal(Value::integer(pname("zebra"), 1, None)),
        );
        c.insert(
            pname("apple"),
            ConfigEntry::literal(Value::integer(pname("apple"), 2, None)),
        );
        let names: Vec<&str> = c.keys().map(ParameterName::as_str).collect();
        assert_eq!(names, vec!["apple", "zebra"]);
    }

    #[test]
    fn exports_insert_and_get() {
        let mut e = Exports::new();
        let n = ExportName::new("service_addr").unwrap();
        let t = TokenExpr::new("${self.ip}:4567").unwrap();
        e.insert(n.clone(), t.clone());
        assert_eq!(e.get(&n), Some(&t));
        assert_eq!(e.len(), 1);
    }

    #[test]
    fn token_expr_serde_roundtrip() {
        let t = TokenExpr::new("${foo.bar}").unwrap();
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"${foo.bar}\"");
        let back: TokenExpr = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn token_expr_deserialise_rejects_empty() {
        let err: Result<TokenExpr, _> = serde_json::from_str("\"\"");
        assert!(err.is_err());
    }

    #[test]
    fn config_entry_serde_roundtrip() {
        let lit = ConfigEntry::literal(Value::integer(pname("n"), 8, None));
        let json = serde_json::to_string(&lit).unwrap();
        let back: ConfigEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(lit, back);
    }
}
