//! Project validation (A§2.3). Owner: meta. `validate` is a signature only.
#![allow(clippy::todo, unused_variables)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::model::Project;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    /// Makes the project invalid.
    Error,
    /// Shown, but the project still loads ("missing media").
    Warning,
}

/// One validation finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Issue {
    pub severity: Severity,
    /// JSON Pointer into `qrec.json`.
    pub path: String,
    pub message: String,
}

/// Every error and warning of A§2.3, in document order.
pub fn validate(project: &Project) -> Vec<Issue> {
    todo!("meta: validate")
}

/// `Ok` when `validate` finds no errors; otherwise `Invalid` with the first
/// error as the message and every issue in `details.issues`.
pub fn ensure_valid(project: &Project) -> Result<(), CoreError> {
    let issues = validate(project);
    match issues.iter().find(|i| i.severity == Severity::Error) {
        None => Ok(()),
        Some(first) => Err(CoreError::invalid(format!(
            "invalid project: {} ({})",
            first.message, first.path
        ))
        .with_details(serde_json::json!({ "issues": issues }))),
    }
}
