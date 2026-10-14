//! Stable identifiers stored in `qrec.json` and used on the command line.
//!
//! Ids are short strings. Qrec generates `L1`, `A1`, `take-0001` and `c7f3`
//! style ids; hand-edited projects may use any short id made of ASCII
//! letters, digits, `-` and `_`, except take ids, whose number drives
//! `nextTakeNumber` and must keep the `take-NNNN` shape.

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::CoreError;

/// A frame index or frame count at the canvas frame rate.
pub type Frame = u64;

fn is_short_id(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && s.len() <= 32
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident, $what:literal, $example:literal, $valid:expr) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = CoreError;

            fn from_str(s: &str) -> Result<Self, CoreError> {
                let valid: fn(&str) -> bool = $valid;
                if valid(s) {
                    Ok(Self(s.to_owned()))
                } else {
                    Err(CoreError::usage(format!(
                        "invalid {} id {s:?} (expected something like {})",
                        $what, $example
                    )))
                }
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                s.parse().map_err(|e: CoreError| serde::de::Error::custom(e.message))
            }
        }
    };
}

string_id!(
    /// A video layer in a scene: `L1`, `L2`, …
    LayerId, "layer", "L1", is_short_id
);
string_id!(
    /// An audio track in a scene: `A1`, `A2`, …
    TrackId, "track", "A1", is_short_id
);
string_id!(
    /// A timeline clip: `c` followed by hex digits, e.g. `c7f3`.
    ClipId, "clip", "c7f3", is_short_id
);
string_id!(
    /// A take (one recording session): `take-0001`. Numbers are never reused.
    TakeId, "take", "take-0001", |s| {
        s.strip_prefix("take-")
            .is_some_and(|n| n.len() >= 4 && n.bytes().all(|b| b.is_ascii_digit()))
    }
);

fn numbered(prefix: char, n: u32) -> String {
    format!("{prefix}{n}")
}

fn number_after(s: &str, prefix: char) -> Option<u32> {
    let digits = s.strip_prefix(prefix)?;
    if digits.starts_with('0') {
        return None;
    }
    digits.parse().ok()
}

impl LayerId {
    /// `L{n}`.
    pub fn numbered(n: u32) -> Self {
        Self(numbered('L', n))
    }

    /// The `n` of an `L{n}` id; `None` for hand-written ids.
    pub fn number(&self) -> Option<u32> {
        number_after(&self.0, 'L')
    }
}

impl TrackId {
    /// `A{n}`.
    pub fn numbered(n: u32) -> Self {
        Self(numbered('A', n))
    }

    /// The `n` of an `A{n}` id; `None` for hand-written ids.
    pub fn number(&self) -> Option<u32> {
        number_after(&self.0, 'A')
    }
}

impl TakeId {
    /// `take-{n:04}`.
    pub fn numbered(n: u32) -> Self {
        Self(format!("take-{n:04}"))
    }

    pub fn number(&self) -> u32 {
        self.0["take-".len()..].parse().unwrap_or(0)
    }
}

impl ClipId {
    /// Builds a clip id without validation. Only `IdGen` and tests should need this.
    pub fn from_raw(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

/// A running job (export, analysis, frame save, purge). Runtime only.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct JobId(pub u64);

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "job-{}", self.0)
    }
}

/// A timer started with `Effect::StartTimer`. Runtime only.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct TimerId(pub u64);

/// The id of a request (CLI invocation, IPC request, GUI action). Chosen by the shell.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ReqId(pub String);

impl fmt::Display for ReqId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_ids_keep_their_shape() {
        assert_eq!(TakeId::numbered(7).as_str(), "take-0007");
        assert_eq!(TakeId::numbered(12345).as_str(), "take-12345");
        assert_eq!("take-0042".parse::<TakeId>().unwrap().number(), 42);
        assert!("take-42".parse::<TakeId>().is_err());
        assert!("take42".parse::<TakeId>().is_err());
    }

    #[test]
    fn short_ids() {
        assert!("c7f3".parse::<ClipId>().is_ok());
        assert!("intro_part-2".parse::<ClipId>().is_ok());
        assert!("".parse::<ClipId>().is_err());
        assert!("-x".parse::<ClipId>().is_err());
        assert!("has space".parse::<LayerId>().is_err());
        assert_eq!(LayerId::numbered(3).number(), Some(3));
        assert_eq!("Face".parse::<LayerId>().unwrap().number(), None);
        assert_eq!("L03".parse::<LayerId>().unwrap().number(), None);
    }

    #[test]
    fn serde_validates() {
        assert!(serde_json::from_str::<TakeId>("\"take-0001\"").is_ok());
        assert!(serde_json::from_str::<TakeId>("\"oops\"").is_err());
    }
}
