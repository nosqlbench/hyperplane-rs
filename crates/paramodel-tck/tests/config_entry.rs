// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ConfigEntry`, `ResolvedConfiguration`, `TokenExpr`, and
//! `Configuration` coverage.

use paramodel_elements::{
    ConfigEntry, Configuration, ParameterName, ResolvedConfiguration, TokenExpr, Value,
};

fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}

// ---------------------------------------------------------------------------
// ConfigEntry literal vs token discrimination.
// ---------------------------------------------------------------------------

#[test]
fn config_entry_literal_is_not_token() {
    let entry = ConfigEntry::literal(Value::integer(pname("n"), 42, None));
    assert!(!entry.is_token());
}

#[test]
fn config_entry_token_is_token() {
    let entry = ConfigEntry::token(TokenExpr::new("${self.ip}").unwrap());
    assert!(entry.is_token());
}

#[test]
fn token_expr_rejects_empty_string() {
    assert!(TokenExpr::new("").is_err());
}

#[test]
fn token_expr_accepts_non_empty() {
    let t = TokenExpr::new("${self.ip}").unwrap();
    assert_eq!(t.as_str(), "${self.ip}");
}

// ---------------------------------------------------------------------------
// ResolvedConfiguration.
// ---------------------------------------------------------------------------

#[test]
fn resolved_configuration_insert_and_get() {
    let mut cfg = ResolvedConfiguration::new();
    cfg.insert(pname("threads"), Value::integer(pname("threads"), 4, None));
    assert_eq!(
        cfg.get(&pname("threads")).and_then(Value::as_integer),
        Some(4),
    );
}

#[test]
fn resolved_configuration_get_missing_returns_none() {
    let cfg = ResolvedConfiguration::new();
    assert!(cfg.get(&pname("absent")).is_none());
}

#[test]
fn resolved_configuration_roundtrips_serde() {
    let mut cfg = ResolvedConfiguration::new();
    cfg.insert(pname("threads"), Value::integer(pname("threads"), 4, None));
    let json = serde_json::to_string(&cfg).unwrap();
    let back: ResolvedConfiguration = serde_json::from_str(&json).unwrap();
    assert_eq!(
        cfg.get(&pname("threads")).and_then(Value::as_integer),
        back.get(&pname("threads")).and_then(Value::as_integer),
    );
}

// ---------------------------------------------------------------------------
// Configuration (entries).
// ---------------------------------------------------------------------------

#[test]
fn configuration_insert_iter_empty() {
    let c = Configuration::new();
    assert_eq!(c.iter().count(), 0);
}

#[test]
fn configuration_insert_iter_records_literals_and_tokens() {
    let mut c = Configuration::new();
    c.insert(
        pname("threads"),
        ConfigEntry::literal(Value::integer(pname("threads"), 4, None)),
    );
    c.insert(
        pname("host"),
        ConfigEntry::token(TokenExpr::new("${self.ip}").unwrap()),
    );
    assert_eq!(c.iter().count(), 2);
    let host_is_token = c
        .iter()
        .find(|(k, _)| k.as_str() == "host")
        .is_some_and(|(_, e)| e.is_token());
    assert!(host_is_token);
}
