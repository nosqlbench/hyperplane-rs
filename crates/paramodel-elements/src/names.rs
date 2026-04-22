// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Validated name newtypes.
//!
//! Names of parameters, elements, axes, plugs, and sockets all share the
//! same shape: a validated UTF-8 identifier. The `Name` trait captures
//! the validation contract; each domain name is a distinct newtype so
//! the compiler can catch "passed an `ElementName` where a `ParameterName`
//! was expected" at call sites.

/// Errors from name construction.
///
/// Kept separate from the crate's top-level `Error` so it can be reused
/// in lower layers without a cyclic reference.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NameError {
    /// The candidate name was empty or made of only whitespace.
    #[error("name must not be empty")]
    Empty,

    /// The candidate name was longer than the permitted byte length.
    #[error("name is {length} bytes, exceeds maximum of {max}")]
    TooLong { length: usize, max: usize },

    /// The candidate name contained a character outside the allowed class
    /// for this name kind.
    #[error("name contains invalid character '{ch}' at byte offset {offset}")]
    InvalidChar { ch: char, offset: usize },

    /// The first character must be an ASCII letter or underscore.
    #[error("name must start with a letter or underscore, got '{ch}'")]
    BadStart { ch: char },
}

/// The validation contract shared by every name kind in the system.
///
/// Implementers pick the permitted character class and length cap by
/// overriding [`Self::validate_char`] and [`Self::MAX_LEN`]. The default
/// [`Self::validate`] implementation enforces the shared "non-empty,
/// legal identifier start, every character passes `validate_char`" rule.
pub trait Name: Sized + AsRef<str> {
    /// Human-readable kind label, used in error messages and `Debug`.
    const KIND: &'static str;

    /// Maximum byte length (UTF-8). Default 64 — override when a larger
    /// ceiling is natural (plan names, for example).
    const MAX_LEN: usize = 64;

    /// Called once per character to decide membership.
    ///
    /// The default admits ASCII identifier characters plus `-` and `.`,
    /// which is a reasonable class for almost every name kind we ship.
    fn validate_char(offset: usize, ch: char) -> Result<(), NameError> {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
            Ok(())
        } else {
            Err(NameError::InvalidChar { ch, offset })
        }
    }

    /// Validate the given string as a name of this kind.
    ///
    /// Called from each newtype's constructor. Implementations that need
    /// stricter or looser rules override [`Self::validate_char`] and / or
    /// this method wholesale.
    fn validate(s: &str) -> Result<(), NameError> {
        if s.is_empty() {
            return Err(NameError::Empty);
        }
        if s.len() > Self::MAX_LEN {
            return Err(NameError::TooLong {
                length: s.len(),
                max:    Self::MAX_LEN,
            });
        }
        // First char must be a letter or underscore so the name is a
        // well-formed identifier even when it lives in generated code.
        let first = s.chars().next().expect("non-empty checked above");
        if !(first.is_ascii_alphabetic() || first == '_') {
            return Err(NameError::BadStart { ch: first });
        }
        for (offset, ch) in s.char_indices() {
            Self::validate_char(offset, ch)?;
        }
        Ok(())
    }
}

// -----------------------------------------------------------------------
// The macro that stamps out concrete name newtypes.
// -----------------------------------------------------------------------

/// Declare a validated name newtype that implements [`Name`].
///
/// Used across the crate wherever a validated ASCII-identifier newtype
/// is needed (parameter names, element names, label keys, port names,
/// …). Each invocation produces a newtype with:
///
/// - `new`, `as_str`, `into_inner`
/// - `Display`, `Debug`, `PartialEq`/`Eq`/`Hash`/`Ord`/`PartialOrd`
/// - serde `Serialize` / `Deserialize` as a transparent string
/// - `FromStr`, `TryFrom<&str>`, `TryFrom<String>`
#[macro_export]
macro_rules! name_type {
    (
        $(#[$meta:meta])*
        $vis:vis struct $Name:ident {
            kind: $kind:literal
            $(, max_len: $max_len:expr )?
            $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        $vis struct $Name(String);

        impl $Name {
            /// Construct a new $Name, validating the candidate.
            pub fn new(candidate: impl Into<String>) -> Result<Self, $crate::names::NameError> {
                let s = candidate.into();
                <Self as $crate::names::Name>::validate(&s)?;
                Ok(Self(s))
            }

            /// Borrow the inner string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume the name and return the inner string.
            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl $crate::names::Name for $Name {
            const KIND: &'static str = $kind;
            $( const MAX_LEN: usize = $max_len; )?
        }

        impl AsRef<str> for $Name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $Name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl std::fmt::Debug for $Name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({:?})", <Self as $crate::names::Name>::KIND, self.0)
            }
        }

        impl std::str::FromStr for $Name {
            type Err = $crate::names::NameError;
            fn from_str(s: &str) -> Result<Self, $crate::names::NameError> {
                Self::new(s.to_owned())
            }
        }

        impl TryFrom<&str> for $Name {
            type Error = $crate::names::NameError;
            fn try_from(s: &str) -> Result<Self, $crate::names::NameError> {
                Self::new(s.to_owned())
            }
        }

        impl TryFrom<String> for $Name {
            type Error = $crate::names::NameError;
            fn try_from(s: String) -> Result<Self, $crate::names::NameError> {
                Self::new(s)
            }
        }

        impl serde::Serialize for $Name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }

        impl<'de> serde::Deserialize<'de> for $Name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
                let s = String::deserialize(deserializer)?;
                Self::new(s).map_err(serde::de::Error::custom)
            }
        }
    };
}

// -----------------------------------------------------------------------
// Concrete name types.
//
// Further domain-specific names (AxisName, PortName, LabelKey, TagKey,
// FacetKey, ExportName, ...) land here in later crates or later commits,
// but the canonical ones shared across the whole system stay in core.
// -----------------------------------------------------------------------

name_type! {
    /// Name of a parameter within the scope of an element.
    ///
    /// Parameter names are scoped to their owning element — two elements
    /// may share a parameter name without conflict. See SRD-0004.
    pub struct ParameterName { kind: "ParameterName" }
}

name_type! {
    /// Name of an element within a test plan.
    ///
    /// Element names are unique within a plan. See SRD-0007.
    pub struct ElementName { kind: "ElementName" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_names_are_accepted() {
        for s in ["threads", "max_connections", "api-v1", "dataset.main", "_reserved"] {
            ParameterName::new(s).expect(s);
        }
    }

    #[test]
    fn empty_names_are_rejected() {
        assert_eq!(ParameterName::new(""), Err(NameError::Empty));
    }

    #[test]
    fn names_must_start_with_letter_or_underscore() {
        assert!(matches!(
            ParameterName::new("1starts-with-digit"),
            Err(NameError::BadStart { ch: '1' })
        ));
        assert!(matches!(
            ParameterName::new(".leading-dot"),
            Err(NameError::BadStart { ch: '.' })
        ));
    }

    #[test]
    fn names_reject_forbidden_chars() {
        let err = ParameterName::new("has space").unwrap_err();
        assert_eq!(
            err,
            NameError::InvalidChar { ch: ' ', offset: 3 }
        );
    }

    #[test]
    fn names_reject_overlong_candidates() {
        let long = "a".repeat(65);
        let err = ParameterName::new(long).unwrap_err();
        assert_eq!(err, NameError::TooLong { length: 65, max: 64 });
    }

    #[test]
    fn debug_format_includes_kind() {
        let p = ParameterName::new("threads").unwrap();
        assert_eq!(format!("{p:?}"), "ParameterName(\"threads\")");

        let e = ElementName::new("jvector").unwrap();
        assert_eq!(format!("{e:?}"), "ElementName(\"jvector\")");
    }

    #[test]
    fn different_kinds_are_type_distinct() {
        // This is a compile-time check disguised as a test: uncomment
        // the assertion below and the build fails because the two are
        // different types. We keep the test here to document intent.
        let _p = ParameterName::new("x").unwrap();
        let _e = ElementName::new("x").unwrap();
        // assert_eq!(_p, _e);  // would not compile
    }

    #[test]
    fn serde_roundtrip() {
        let name = ParameterName::new("threads").unwrap();
        let json = serde_json::to_string(&name).unwrap();
        assert_eq!(json, "\"threads\"");
        let back: ParameterName = serde_json::from_str(&json).unwrap();
        assert_eq!(name, back);
    }

    #[test]
    fn deserialise_rejects_invalid_names() {
        let err = serde_json::from_str::<ParameterName>("\"has space\"");
        assert!(err.is_err());
    }

    // Dead-simple property: round-trip via Display.
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn valid_names_roundtrip(
            s in "[A-Za-z_][A-Za-z0-9_\\-.]{0,63}"
        ) {
            let name = ParameterName::new(s.clone()).expect(&s);
            prop_assert_eq!(name.as_str(), s.as_str());
        }
    }
}

