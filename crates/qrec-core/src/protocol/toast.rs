//! Notifications for people (A§4.5): GUI toasts, stderr lines for the CLI,
//! `{"event":"notice",…}` lines on stderr under `--json`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::protocol::command::Command;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Toast {
    pub level: ToastLevel,
    pub title: String,
    pub body: Option<String>,
    #[serde(default)]
    pub actions: Vec<ToastAction>,
}

impl Toast {
    pub fn new(level: ToastLevel, title: impl Into<String>) -> Self {
        Self {
            level,
            title: title.into(),
            body: None,
            actions: Vec::new(),
        }
    }

    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn with_action(mut self, label: impl Into<String>, action: UiAction) -> Self {
        self.actions.push(ToastAction {
            label: label.into(),
            action,
        });
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToastAction {
    pub label: String,
    pub action: UiAction,
}

/// What a toast button does. Paths are absolute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum UiAction {
    OpenPath(String),
    RevealPath(String),
    CopyText(String),
    /// Copies a PNG file's image (`wl-copy --type image/png`).
    CopyImage(String),
    /// Runs a command (e.g. "Undo").
    Dispatch(Box<Command>),
}
