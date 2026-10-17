//! GTK's fundamental types are all registered up front
//! (`app::register_fundamentals`, before any worker starts): GLib gives out
//! fundamental type IDs in two steps, so GStreamer's init on its own thread
//! once took GTK's (WP10's scale runs: "cannot register existing
//! fundamental type 'GstFlagSet' (as 'GdkEvent')", the window never
//! opened). Its own test binary: a fresh type system, no other test
//! registering GTK's types first.

use gtk::glib::gobject_ffi::g_type_fundamental_next;

/// The next fundamental type ID GLib would hand out.
fn next() -> gtk::glib::ffi::GType {
    // SAFETY: reads GLib's counter; no arguments.
    unsafe { g_type_fundamental_next() }
}

/// After `register_fundamentals`, registering every GTK type (GDK's and
/// GSK's with GTK's: `gtk_test_register_all_types`) takes no fundamental ID:
/// GTK registers none the function leaves out.
#[test]
fn every_gtk_fundamental_type_is_registered_up_front() {
    let before = next();
    qrec_gui::app::register_fundamentals();
    let registered = next();
    assert!(
        registered > before,
        "GTK's fundamental types weren't registered"
    );
    // SAFETY: registers types only; GTK needn't be initialized for it
    // (gtk-rs's wrapper insists on it).
    unsafe { gtk::ffi::gtk_test_register_all_types() };
    assert_eq!(
        next(),
        registered,
        "GTK registered a fundamental type `register_fundamentals` leaves out"
    );
}
