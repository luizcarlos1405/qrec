//! In-process commands (A§8.5): with no host, the CLI process takes the
//! lock and **is** the host (`HostKind::Cli`) until its command is done,
//! serving IPC while async jobs run. Owner: ipc. Empty stub.
