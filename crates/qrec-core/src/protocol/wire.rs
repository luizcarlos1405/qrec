//! The wire format of IPC and `--json` (A§4.9): JSON lines, `v: 1`, one
//! request per connection.
//!
//! ```text
//! client → host  {"v":1,"id":"7","confirmed":false,"body":{"command":{"cmd":"clipSplit","clip":"c7f3","at":"13f"}}}
//!                {"v":1,"id":"9","body":"hello"}
//! host → client  {"id":"7","accepted":true}
//!                {"id":"7","progress":{"phase":"render","done":1234,"total":5678,"etaSecs":42,"label":"Rendering clip 4/12"}}
//!                {"id":"7","reply":{"ok":true,"data":{…}}}
//! ```

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::CoreError;
use crate::protocol::command::Command;
use crate::protocol::progress::Progress;
use crate::protocol::query::Query;
use crate::state::{HostKind, Job, RecordPhase};

/// The protocol version in every request (`"v"`).
pub const WIRE_VERSION: u32 = 1;

/// One client request line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireRequest {
    pub v: u32,
    /// The client's id; echoed in every message about this request.
    pub id: String,
    /// `--yes`.
    #[serde(default)]
    pub confirmed: bool,
    pub body: WireBody,
}

/// What the client asks for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WireBody {
    Command(Command),
    Query(Query),
    /// → `HelloInfo`.
    Hello,
    /// Stop accepting, let jobs finish, exit (A§8.6).
    Shutdown,
}

/// One host message line. Exactly one of `accepted`, `progress` or `reply`
/// besides the `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum WireMessage {
    /// Sent once the request parsed.
    Accepted {
        id: String,
        accepted: bool,
    },
    Progress {
        id: String,
        progress: Progress,
    },
    /// `reply` is [`to_wire`] of the result.
    Reply {
        id: String,
        reply: Value,
    },
}

impl WireMessage {
    pub fn accepted(id: impl Into<String>) -> Self {
        WireMessage::Accepted {
            id: id.into(),
            accepted: true,
        }
    }

    pub fn progress(id: impl Into<String>, progress: Progress) -> Self {
        WireMessage::Progress {
            id: id.into(),
            progress,
        }
    }

    pub fn reply(id: impl Into<String>, result: &Result<Value, CoreError>) -> Self {
        WireMessage::Reply {
            id: id.into(),
            reply: to_wire(result),
        }
    }

    pub fn id(&self) -> &str {
        match self {
            WireMessage::Accepted { id, .. }
            | WireMessage::Progress { id, .. }
            | WireMessage::Reply { id, .. } => id,
        }
    }
}

/// The reply to `hello`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelloInfo {
    pub pid: u32,
    /// The canonical project folder, for people (lossy when the path isn't
    /// UTF-8: then [`HelloInfo::project_dir_hex`] has the real one).
    pub project_dir: String,
    /// The folder's exact bytes, in hex, only when they aren't UTF-8: a
    /// client that uses this host's project (A§8.4) rebuilds the path from
    /// them ([`HelloInfo::project_dir_bytes`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir_hex: Option<String>,
    pub host: HostKind,
    pub record: RecordPhase,
    pub jobs: Vec<Job>,
    /// The Qrec version of the host.
    pub version: String,
}

impl HelloInfo {
    /// `(project_dir, project_dir_hex)` for a folder whose path is `bytes`.
    pub fn dir_fields(bytes: &[u8]) -> (String, Option<String>) {
        match std::str::from_utf8(bytes) {
            Ok(text) => (text.to_owned(), None),
            Err(_) => (
                String::from_utf8_lossy(bytes).into_owned(),
                Some(crate::hash::to_hex(bytes)),
            ),
        }
    }

    /// The project folder's exact path bytes.
    pub fn project_dir_bytes(&self) -> Vec<u8> {
        self.project_dir_hex
            .as_deref()
            .and_then(crate::hash::from_hex)
            .unwrap_or_else(|| self.project_dir.as_bytes().to_vec())
    }
}

/// `{"ok":true,"data":…}` or `{"ok":false,"error":{"code","message","hint"?,"details"?}}`:
/// the object `--json` prints and IPC replies carry.
pub fn to_wire(result: &Result<Value, CoreError>) -> Value {
    match result {
        Ok(data) => json!({ "ok": true, "data": data }),
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

/// The inverse of [`to_wire`]; `Err` describes a malformed reply.
pub fn from_wire(value: &Value) -> Result<Result<Value, CoreError>, String> {
    match value.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(Ok(value.get("data").cloned().unwrap_or(Value::Null))),
        Some(false) => {
            let error = value
                .get("error")
                .cloned()
                .ok_or("reply without \"error\"")?;
            serde_json::from_value(error)
                .map(Err)
                .map_err(|e| format!("bad error object: {e}"))
        }
        None => Err("reply without a boolean \"ok\"".to_owned()),
    }
}
