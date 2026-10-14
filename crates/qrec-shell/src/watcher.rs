//! External-edit watcher (A§8.9): inotify on the project directory
//! (`IN_CLOSE_WRITE|IN_MOVED_TO|IN_CREATE|IN_DELETE`), filtered on
//! `qrec.json`, debounced 200 ms and compared by sha256 with what the host
//! last wrote; posts `Msg::DiskChanged`. Owner: store. Empty stub.
