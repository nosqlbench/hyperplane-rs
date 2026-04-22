// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Pagination primitives + stream alias.
//!
//! Per SRD-0012 D3: cursor-based pagination via opaque `PageToken`;
//! no offset/limit. Backends clamp `PageRequest::size` to a hard max.
//! Per D15: `PersistenceStream<T>` is owned and `'static` so
//! backends can move shared state into the stream before pinning.

use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::error::PersistenceResult;

/// Suggested upper bound on `PageRequest::size`. Backends may cap
/// lower; they must not exceed this.
pub const MAX_PAGE_SIZE: u32 = 1000;

/// Request for one page of records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRequest {
    /// Maximum records to return. Clamped to `MAX_PAGE_SIZE`.
    pub size:  u32,
    /// Cursor from a previous page. `None` for the first page.
    pub token: Option<PageToken>,
}

impl PageRequest {
    /// Construct a first-page request with the given size.
    #[must_use]
    pub const fn first(size: u32) -> Self {
        Self { size, token: None }
    }

    /// Construct a follow-up page request.
    #[must_use]
    pub const fn after(size: u32, token: PageToken) -> Self {
        Self {
            size,
            token: Some(token),
        }
    }

    /// Clamp `size` to `MAX_PAGE_SIZE`.
    #[must_use]
    pub const fn clamped(mut self) -> Self {
        if self.size > MAX_PAGE_SIZE {
            self.size = MAX_PAGE_SIZE;
        } else if self.size == 0 {
            self.size = 1;
        }
        self
    }
}

impl Default for PageRequest {
    fn default() -> Self {
        Self::first(100)
    }
}

/// Opaque cursor token. The backend chooses the encoding.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PageToken(String);

impl PageToken {
    /// Wrap a backend-chosen encoding.
    #[must_use]
    pub const fn new(raw: String) -> Self {
        Self(raw)
    }

    /// Borrow the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume and return the underlying string.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

/// One page of records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    /// Records in this page, in backend-defined order.
    pub items:      Vec<T>,
    /// Cursor to retrieve the next page; `None` when exhausted.
    pub next_token: Option<PageToken>,
    /// Optional server-side count estimate across the filter. `None`
    /// when the backend declines to estimate.
    pub total_hint: Option<u64>,
}

impl<T> Page<T> {
    /// Build an empty terminal page.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            items:      Vec::new(),
            next_token: None,
            total_hint: None,
        }
    }

    /// Build a final page with the given items.
    #[must_use]
    pub const fn terminal(items: Vec<T>) -> Self {
        Self {
            items,
            next_token: None,
            total_hint: None,
        }
    }
}

impl<T> Default for Page<T> {
    fn default() -> Self {
        Self::empty()
    }
}

/// Owned `'static` stream of persistence records.
///
/// Items are `PersistenceResult<T>` so partial scans can recover:
/// a mid-stream failure surfaces as one `Err` item and the stream
/// may continue.
pub type PersistenceStream<T> = BoxStream<'static, PersistenceResult<T>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_request_default_is_first_page_hundred() {
        let r = PageRequest::default();
        assert_eq!(r.size, 100);
        assert!(r.token.is_none());
    }

    #[test]
    fn page_request_clamped_caps_large_size() {
        let r = PageRequest::first(10_000).clamped();
        assert_eq!(r.size, MAX_PAGE_SIZE);
    }

    #[test]
    fn page_request_clamped_floors_zero() {
        let r = PageRequest::first(0).clamped();
        assert_eq!(r.size, 1);
    }

    #[test]
    fn page_token_roundtrips() {
        let t = PageToken::new("abc123".to_owned());
        assert_eq!(t.as_str(), "abc123");
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"abc123\"");
        let back: PageToken = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }
}
