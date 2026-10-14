//! Session tests.

use super::*;

#[test]
fn sessions_move_between_threads() {
    fn send<T: Send>() {}
    send::<Session>();
}
