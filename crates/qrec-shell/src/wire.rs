//! The wire format (A§4.9) as the shell uses it: the types live in the core
//! (`qrec_core::protocol::wire`) and are re-exported here for the IPC layer
//! and the CLI. JSON-lines framing, limits and timeouts (A§8.3) belong to
//! `ipc`.

pub use qrec_core::protocol::wire::{
    HelloInfo, WIRE_VERSION, WireBody, WireMessage, WireRequest, from_wire, to_wire,
};
