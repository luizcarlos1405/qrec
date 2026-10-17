//! Following the user's settings live (G§3.14, WP10): `qrec config set
//! theme <t>`, `ui.scale` and `ui.reduced_motion` apply to the open window,
//! and the keymap reloads when `keys.toml` changes, with its problems as
//! toasts.
//!
//! The shell's settings watcher (`qrec_shell::watcher::watch_settings`)
//! watches the config dir from its own thread and reads both files there
//! (R1: no file I/O here); what it says comes through the queue
//! (`Inbound::Settings`), in order:
//! - `Pending`, a burst of file events began: the follower holds an io
//!   token (`MediaHub::hold_io_busy`) until the change is applied, so the
//!   test channel's `waitIdle` waits for it, at most [`PENDING_LIMIT`];
//! - `Settled`, the burst was quiet for 200 ms: [`plan`] decides what
//!   changes, and the follower applies it: the look through
//!   [`Theme::apply`] (the palette, the tokens provider, the CSS at the new
//!   scale, libadwaita's color scheme, every custom widget through
//!   `Theme::subscribe`, the motion policy and with it the window's
//!   `.reduced-motion`), the keymap through `Store::set_keymap` (the key
//!   router, the hint bar and every key cap read it), and the toasts: a
//!   `config.toml` that can't be used (once per change of the file, shared
//!   with the Session), and each `keys.toml` problem the window didn't have
//!   (conflicts, unknown ids, bad keys, as the core words them).
//!
//! The system's reduced-motion setting is followed too (D-30): the policy
//! is the config's choice or GTK's `gtk-interface-reduced-motion` and
//! animations setting, whichever asks for less motion (test mode: the
//! config's only).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use qrec_core::config::Theme as ThemeId;
use qrec_core::keymap::Keymap;
use qrec_core::protocol::ToastLevel;
use qrec_core::view::notices::Notice;
use qrec_shell::facts::{ConfigFile, config_problem_toast};
use qrec_shell::paths::UserDirs;
use qrec_shell::watcher::{self, Settings, SettingsEvent, SettingsWatcher};

use crate::host::queue::{Inbound, Queue};
use crate::media::io::BusyToken;
use crate::theme::MotionPolicy;
use crate::{Ctx, guard, guarded};

/// How long a change may stay pending (its debounce and its reads take
/// ≈ 0.2 s; a file that keeps changing settles within 1 s) before the
/// window stops waiting for it (every wait has a deadline, G§3.12 R6).
pub const PENDING_LIMIT: Duration = Duration::from_secs(5);

/// The title of a `keys.toml` problem's toast (as at startup).
pub const KEYS_PROBLEM: &str = "keys.toml has a problem";

/// What the window shows of the settings now.
#[derive(Debug, Clone, Copy)]
pub struct Shown<'a> {
    pub theme: ThemeId,
    pub scale: f64,
    /// `config.toml`'s `ui.reduced_motion` (the system's setting joins it
    /// when it is applied).
    pub reduced_motion: bool,
    pub keymap: &'a Keymap,
    pub keymap_problems: &'a [String],
}

/// What a settled change does to the window ([`plan`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// The look the config asks for (the theme applies it only when it
    /// differs).
    pub theme: ThemeId,
    pub scale: f64,
    pub reduced_motion: bool,
    /// The keymap and its problems, when either changed.
    pub keymap: Option<(Keymap, Vec<String>)>,
    /// The toasts, in order: `config.toml`'s problem, then each `keys.toml`
    /// problem the window didn't have.
    pub notices: Vec<Notice>,
}

impl Plan {
    /// Whether the look changes from `shown`'s.
    pub fn restyles(&self, shown: &Shown<'_>) -> bool {
        self.theme != shown.theme
            || (self.scale - shown.scale).abs() >= f64::EPSILON
            || self.reduced_motion != shown.reduced_motion
    }
}

/// What `settings` (both files as they are now) change in a window that
/// shows `shown` (GTK-free).
pub fn plan(shown: &Shown<'_>, settings: Settings) -> Plan {
    let Settings {
        config,
        config_problem,
        keymap,
        keymap_problems,
    } = settings;
    let mut notices: Vec<Notice> = config_problem
        .map(|problem| Notice::from(config_problem_toast(problem)))
        .into_iter()
        .collect();
    let keymap = if keymap != *shown.keymap || keymap_problems != shown.keymap_problems {
        notices.extend(
            keymap_problems
                .iter()
                .filter(|p| !shown.keymap_problems.contains(p))
                .map(|p| Notice::new(ToastLevel::Warning, KEYS_PROBLEM).with_body(p.clone())),
        );
        Some((keymap, keymap_problems))
    } else {
        None
    };
    Plan {
        theme: config.theme,
        scale: config.ui.scale,
        reduced_motion: config.ui.reduced_motion,
        keymap,
        notices,
    }
}

/// The window's settings follower. Main thread only; dropping it stops
/// the watcher.
pub struct Follower {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    /// `config.toml`'s `ui.reduced_motion` as last applied.
    reduced_motion: Cell<bool>,
    /// Held while a change is pending (`waitIdle` waits for it).
    busy: RefCell<Option<BusyToken>>,
    /// The pending change's deadline.
    deadline: guard::Slot,
    /// How many changes were applied (the test channel's `local`).
    applied: Cell<u64>,
    watcher: RefCell<Option<SettingsWatcher>>,
}

impl Follower {
    /// Follows the config dir of `dirs` (`config.toml` through `config`,
    /// the `ConfigFile` the Session gets too) from now on; `reduced_motion`
    /// is the config's as startup read it. Without user dirs nothing is
    /// watched: the settings stay as startup read them.
    pub fn start(
        ctx: &Ctx,
        queue: &Queue,
        dirs: Option<&UserDirs>,
        config: Option<&ConfigFile>,
        reduced_motion: bool,
    ) -> Follower {
        let inner = Rc::new(Inner {
            ctx: ctx.clone(),
            reduced_motion: Cell::new(reduced_motion),
            busy: RefCell::new(None),
            deadline: guard::Slot::new(),
            applied: Cell::new(0),
            watcher: RefCell::new(None),
        });
        if let (Some(dirs), Some(config)) = (dirs, config) {
            let queue = queue.clone();
            let started = watcher::watch_settings(&dirs.config, config.clone(), move |event| {
                queue.push(Inbound::Settings(event));
            });
            match started {
                Ok(watcher) => *inner.watcher.borrow_mut() = Some(watcher),
                Err(e) => tracing::warn!("the settings won't follow changes: {e}"),
            }
        }
        inner.follow_system_motion();
        Follower { inner }
    }

    /// What the settings watcher said (`Inbound::Settings`).
    pub fn on_event(&self, event: SettingsEvent) {
        match event {
            SettingsEvent::Pending => self.inner.pending(),
            SettingsEvent::Settled(settings) => self.inner.settled(*settings),
        }
    }

    /// How many settled changes were applied.
    pub fn applied(&self) -> u64 {
        self.inner.applied.get()
    }

    /// Whether a change is pending.
    pub fn pending(&self) -> bool {
        self.inner.busy.borrow().is_some()
    }
}

impl Inner {
    fn pending(self: &Rc<Self>) {
        self.busy
            .borrow_mut()
            .get_or_insert_with(|| self.ctx.media.hold_io_busy());
        if self.deadline.is_running() {
            return;
        }
        let weak = Rc::downgrade(self);
        self.deadline
            .timeout_once(PENDING_LIMIT, "settings: a change's deadline", move || {
                if let Some(inner) = weak.upgrade()
                    && inner.busy.borrow_mut().take().is_some()
                {
                    tracing::warn!(
                        "settings: a change didn't settle within {} s",
                        PENDING_LIMIT.as_secs()
                    );
                }
            });
    }

    fn settled(&self, settings: Settings) {
        // The token goes once the change is applied (and with a panic too).
        let busy = self.busy.borrow_mut().take();
        self.deadline.stop();
        let theme = &self.ctx.theme;
        let store = &self.ctx.store;
        let keymap = store.keymap();
        let problems = store.local().keymap_problems.clone();
        let shown = Shown {
            theme: theme.id(),
            scale: theme.scale(),
            reduced_motion: self.reduced_motion.get(),
            keymap: &keymap,
            keymap_problems: &problems,
        };
        let plan = plan(&shown, settings);
        let restyles = plan.restyles(&shown);
        self.applied.set(self.applied.get() + 1);
        if restyles {
            self.reduced_motion.set(plan.reduced_motion);
            theme.apply(
                plan.theme,
                plan.scale,
                MotionPolicy::from_settings(plan.reduced_motion),
            );
        }
        if let Some((keymap, problems)) = plan.keymap {
            tracing::info!(
                "keys.toml reloaded ({} problem{})",
                problems.len(),
                if problems.len() == 1 { "" } else { "s" }
            );
            store.set_keymap(keymap, problems);
        }
        for notice in plan.notices {
            store.notify(notice);
        }
        drop(busy);
    }

    /// The system's reduced-motion setting and GTK's animations, live
    /// (D-30): the policy is recomputed with the config's choice.
    fn follow_system_motion(self: &Rc<Self>) {
        let Some(settings) = gtk::Settings::default() else {
            return;
        };
        let weak = Rc::downgrade(self);
        settings.connect_gtk_interface_reduced_motion_notify(guarded!(
            "settings: the system's reduced motion",
            move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.motion_changed();
                }
            }
        ));
        let weak = Rc::downgrade(self);
        settings.connect_gtk_enable_animations_notify(guarded!(
            "settings: GTK's animations",
            move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.motion_changed();
                }
            }
        ));
    }

    fn motion_changed(&self) {
        let motion = MotionPolicy::from_settings(self.reduced_motion.get());
        self.ctx.theme.set_motion(motion);
    }
}

#[cfg(test)]
mod tests {
    use qrec_core::config::UserConfig;
    use qrec_core::keymap;

    use super::*;

    fn settings(config: UserConfig, keys: &str) -> Settings {
        let keymap = keymap::resolve(keys);
        let keymap_problems = keymap::problems(&keymap);
        Settings {
            config,
            config_problem: None,
            keymap,
            keymap_problems,
        }
    }

    fn shown<'a>(keymap: &'a Keymap, problems: &'a [String]) -> Shown<'a> {
        Shown {
            theme: ThemeId::SwampNight,
            scale: 1.0,
            reduced_motion: false,
            keymap,
            keymap_problems: problems,
        }
    }

    /// The same settings change nothing and toast nothing.
    #[test]
    fn unchanged_settings_are_no_change() {
        let keymap = keymap::resolve("");
        let shown = shown(&keymap, &[]);
        let plan = plan(&shown, settings(UserConfig::default(), ""));
        assert!(!plan.restyles(&shown));
        assert_eq!(plan.keymap, None);
        assert!(plan.notices.is_empty());
    }

    /// `qrec config set theme | ui.scale | ui.reduced_motion`: the look the
    /// config asks for, the keymap untouched.
    #[test]
    fn the_look_follows_the_config() {
        let keymap = keymap::resolve("");
        let shown = shown(&keymap, &[]);
        let config = UserConfig {
            theme: ThemeId::SwampDay,
            ui: qrec_core::config::UiDefaults {
                scale: 1.5,
                reduced_motion: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let plan = plan(&shown, settings(config, ""));
        assert!(plan.restyles(&shown));
        assert_eq!(
            (plan.theme, plan.scale, plan.reduced_motion),
            (ThemeId::SwampDay, 1.5, true)
        );
        assert_eq!(plan.keymap, None);
        assert!(plan.notices.is_empty());
    }

    /// A rebinding is a new keymap, quietly; a conflict and an unknown id
    /// are a toast each, as the core words them, and only the problems the
    /// window didn't have: a later edit that keeps them toasts nothing.
    #[test]
    fn a_new_keymap_toasts_its_new_problems() {
        let defaults = keymap::resolve("");
        let shown_defaults = shown(&defaults, &[]);
        let rebound = plan(
            &shown_defaults,
            settings(
                UserConfig::default(),
                "[bindings]\n\"bin.toggle\" = \"F8\"\n",
            ),
        );
        let (keymap, problems) = rebound.keymap.expect("a new keymap");
        assert_eq!(
            keymap.binding("bin.toggle").unwrap().keys[0].to_string(),
            "F8"
        );
        assert!(problems.is_empty());
        assert!(rebound.notices.is_empty());

        let keys = "[bindings]\n\"autocut.run\" = \"x\"\n\"nope.nothing\" = \"q\"\n";
        let broken = plan(&shown_defaults, settings(UserConfig::default(), keys));
        let (keymap, problems) = broken.keymap.expect("a new keymap");
        assert_eq!(problems.len(), 2, "{problems:?}");
        let bodies: Vec<_> = broken.notices.iter().map(|n| n.body.clone()).collect();
        assert_eq!(
            bodies,
            problems.iter().cloned().map(Some).collect::<Vec<_>>()
        );
        for notice in &broken.notices {
            assert_eq!(notice.title, KEYS_PROBLEM);
            assert_eq!(notice.level, ToastLevel::Warning);
        }
        assert!(problems[0].starts_with("x is bound to clip.remove and autocut.run"));
        assert!(problems[1].contains("unknown binding `nope.nothing`"));

        // Another edit that keeps the conflict and fixes the unknown id:
        // the new keymap, no toast.
        let shown_broken = shown(&keymap, &problems);
        let kept = plan(
            &shown_broken,
            settings(
                UserConfig::default(),
                "[bindings]\n\"autocut.run\" = \"x\"\n",
            ),
        );
        let (_, kept_problems) = kept.keymap.expect("a new keymap");
        assert_eq!(kept_problems, problems[..1]);
        assert!(kept.notices.is_empty(), "{:?}", kept.notices);
    }

    /// A `config.toml` that can't be used: the defaults, and its warning
    /// (the Session's own toast, once per change of the file).
    #[test]
    fn a_config_problem_is_told() {
        let keymap = keymap::resolve("");
        let shown = Shown {
            theme: ThemeId::OgreMode,
            ..shown(&keymap, &[])
        };
        let mut broken = settings(UserConfig::default(), "");
        broken.config_problem =
            Some("config.toml line 1: unknown variant; using the default settings".into());
        let plan = plan(&shown, broken);
        assert!(plan.restyles(&shown), "back to the defaults");
        assert_eq!(plan.theme, ThemeId::SwampNight);
        assert_eq!(plan.notices.len(), 1);
        let notice = &plan.notices[0];
        assert_eq!(notice.title, qrec_shell::facts::CONFIG_INVALID);
        assert_eq!(notice.level, ToastLevel::Warning);
        assert!(notice.body.as_deref().unwrap().contains("unknown variant"));
    }

    /// `keys.toml` unreadable while the defaults were in use: the same
    /// keymap, a new problem, toasted.
    #[test]
    fn an_unreadable_keys_toml_is_told_even_with_the_same_keymap() {
        let keymap = keymap::resolve("");
        let shown = shown(&keymap, &[]);
        let mut unreadable = settings(UserConfig::default(), "");
        unreadable.keymap_problems =
            vec!["can't read keys.toml: Is a directory; using the default keys".into()];
        let plan = plan(&shown, unreadable);
        let (_, problems) = plan.keymap.expect("new problems");
        assert_eq!(problems.len(), 1);
        assert_eq!(plan.notices.len(), 1);
    }
}
