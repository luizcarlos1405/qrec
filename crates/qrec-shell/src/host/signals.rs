//! Signals in hosts (A§8.6): the first SIGTERM/SIGINT/SIGHUP dispatches
//! `RecordStop` (finalize, not cancel) and cancels jobs; a second one
//! SIGKILLs child process groups and `_exit(130)`. Owner: ipc. Empty stub.
