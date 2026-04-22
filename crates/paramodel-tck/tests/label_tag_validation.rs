// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `LabelKey` / `LabelValue` / `TagKey` / `TagValue` validation.

use paramodel_elements::{LabelKey, LabelValue, TagKey, TagValue};

// ---------------------------------------------------------------------------
// Non-empty requirement.
// ---------------------------------------------------------------------------

#[test]
fn label_key_rejects_empty() {
    assert!(LabelKey::new("").is_err());
}

#[test]
fn label_value_rejects_empty() {
    assert!(LabelValue::new("").is_err());
}

#[test]
fn tag_key_rejects_empty() {
    assert!(TagKey::new("").is_err());
}

#[test]
fn tag_value_rejects_empty() {
    assert!(TagValue::new("").is_err());
}

// ---------------------------------------------------------------------------
// Character-class enforcement.
// ---------------------------------------------------------------------------

#[test]
fn label_key_accepts_allowed_punctuation() {
    // LabelKeys are ASCII-identifier-like: letters, digits, '-', '.',
    // '_'. Slashes and colons are NOT allowed.
    assert!(LabelKey::new("owner").is_ok());
    assert!(LabelKey::new("app.name").is_ok());
    assert!(LabelKey::new("kube-app").is_ok());
    assert!(LabelKey::new("kube_app").is_ok());
    assert!(LabelKey::new("app.kubernetes.io/name").is_err());
}

#[test]
fn label_key_rejects_whitespace() {
    assert!(LabelKey::new("has space").is_err());
    assert!(LabelKey::new("has\ttab").is_err());
    assert!(LabelKey::new("has\nnewline").is_err());
}

#[test]
fn label_value_rejects_control_characters() {
    // NUL is always rejected.
    assert!(LabelValue::new("\0").is_err());
    assert!(LabelValue::new("bad\0value").is_err());
}

// ---------------------------------------------------------------------------
// Round-trip and display.
// ---------------------------------------------------------------------------

#[test]
fn label_key_display_matches_input() {
    let k = LabelKey::new("env").unwrap();
    assert_eq!(k.as_str(), "env");
    assert_eq!(format!("{k}"), "env");
}

#[test]
fn label_value_preserves_unicode() {
    let v = LabelValue::new("日本語").unwrap();
    assert_eq!(v.as_str(), "日本語");
}
