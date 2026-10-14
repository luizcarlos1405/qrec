//! Schema migrations `vN → vN+1` (SPEC §6.3). Owner: meta. Stub: signatures only.
//!
//! The shell writes `.qrec/qrec.v{N}.json.bak` (never overwritten) before
//! saving a migrated project.
#![allow(clippy::todo, unused_variables)]

use serde_json::Value;

use crate::error::CoreError;
use crate::model::Project;

/// A project read from disk, at `CURRENT_VERSION`.
#[derive(Debug, Clone, PartialEq)]
pub struct Migrated {
    pub project: Project,
    /// The version found in the file (`< CURRENT_VERSION` ⇒ it was migrated).
    pub from_version: u32,
}

/// Migrates a parsed `qrec.json` to `CURRENT_VERSION` and deserializes it.
/// A newer version than this build knows is `Invalid`.
pub fn migrate(value: Value) -> Result<Migrated, CoreError> {
    todo!("meta: migrate")
}

/// Parses `qrec.json` text, then [`migrate`]s it.
pub fn parse_project(text: &str) -> Result<Migrated, CoreError> {
    todo!("meta: parse_project")
}
