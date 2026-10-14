//! The spawn hand-off (A§8.3 step 5): the CLI takes the lock, spawns the
//! headless host with the lock fd and a ready fd (`setsid`, stdio to
//! `.qrec/logs/…-host.log`) and never unlocks. Owner: ipc. Empty stub.
