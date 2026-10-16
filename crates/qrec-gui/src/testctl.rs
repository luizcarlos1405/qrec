//! Test mode and the test control channel (G§3.18, G§8.4).
//!
//! **Test mode** (`QREC_GUI_TEST`, read once): `1` (or any value but `0`)
//! turns it on: GTK's animations and the cursor blink are off, the
//! animation clock is frozen at `QREC_GUI_TEST_T_MS` (default 0), one-shot
//! animations jump to their end, reduced motion comes from the config only
//! (D-30), `QREC_GUI_TEST_NOW` (RFC 3339) is the local time of the export's
//! auto name, and the control channel listens. Flags, comma-separated:
//! `stuck-gst` makes startup report GStreamer stuck without initializing it
//! (the stuck page, exit 5).
//!
//! **The control channel** (WP0b part 2): a socket
//! `$XDG_RUNTIME_DIR/qrec-gui-test.sock` (0600), one JSON request line and
//! one JSON reply line; requests reach the main thread as
//! `Inbound::Test`. Ops: `waitIdle`, `regions`, `dispatch`, `action`,
//! `hints`, `local`, `a11y`. Dispatches go through the Store (R8); the
//! others only read.

use std::sync::OnceLock;

use chrono::{DateTime, NaiveDateTime};
use serde_json::Value;

/// `QREC_GUI_TEST` and its companions, as read at startup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestMode {
    /// Test mode is on.
    pub on: bool,
    /// `stuck-gst`: report GStreamer stuck without initializing it.
    pub stuck_gst: bool,
    /// The frozen animation clock (`QREC_GUI_TEST_T_MS`).
    pub t_ms: u64,
    /// The local time of the export's auto name (`QREC_GUI_TEST_NOW`: RFC
    /// 3339, its offset ignored, or `YYYY-MM-DDTHH:MM:SS`).
    pub now: Option<NaiveDateTime>,
}

impl TestMode {
    /// From the variables' values.
    pub fn parse(test: Option<&str>, t_ms: Option<&str>, now: Option<&str>) -> Self {
        let flags: Vec<&str> = test
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .collect();
        let on = !flags.is_empty() && flags != ["0"];
        Self {
            on,
            stuck_gst: on && flags.contains(&"stuck-gst"),
            t_ms: t_ms.and_then(|t| t.trim().parse().ok()).unwrap_or(0),
            now: now.and_then(|n| {
                let n = n.trim();
                DateTime::parse_from_rfc3339(n)
                    .map(|t| t.naive_local())
                    .or_else(|_| NaiveDateTime::parse_from_str(n, "%Y-%m-%dT%H:%M:%S"))
                    .ok()
            }),
        }
    }

    fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok();
        Self::parse(
            var("QREC_GUI_TEST").as_deref(),
            var("QREC_GUI_TEST_T_MS").as_deref(),
            var("QREC_GUI_TEST_NOW").as_deref(),
        )
    }
}

/// The process's test mode (read on first use).
pub fn mode() -> &'static TestMode {
    static MODE: OnceLock<TestMode> = OnceLock::new();
    MODE.get_or_init(TestMode::from_env)
}

/// One control request on its way to the main thread, with where its reply
/// goes. (The channel itself is WP0b part 2.)
pub struct TestRequest {
    /// The request line, parsed.
    pub request: Value,
    /// Called once with the reply line's JSON.
    pub reply: Box<dyn FnOnce(Value) + Send>,
}

impl std::fmt::Debug for TestRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestRequest")
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mode_parses_its_flags() {
        assert_eq!(TestMode::parse(None, None, None), TestMode::default());
        assert!(!TestMode::parse(Some(""), None, None).on);
        assert!(!TestMode::parse(Some("0"), None, None).on);
        let on = TestMode::parse(Some("1"), Some("250"), Some("2026-10-14T15:30:12+00:00"));
        assert!(on.on && !on.stuck_gst);
        assert_eq!(on.t_ms, 250);
        assert_eq!(on.now.unwrap().to_string(), "2026-10-14 15:30:12");
        let naive = TestMode::parse(Some("1"), None, Some("2026-10-14T15:30:12"));
        assert_eq!(naive.now, on.now);
        let stuck = TestMode::parse(Some("stuck-gst"), Some("x"), Some("yesterday"));
        assert!(stuck.on && stuck.stuck_gst);
        assert_eq!((stuck.t_ms, stuck.now), (0, None));
        assert!(TestMode::parse(Some("1,stuck-gst"), None, None).stuck_gst);
    }
}
