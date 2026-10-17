//! GUI actions by id (G§3.8): what a binding whose `keymap::action_for` is
//! `Action::Gui` does, and the GUI overrides of step 10. Each region
//! registers its ids in its own constructor:
//!
//! | ids | registered by |
//! | --- | --- |
//! | `zoom.in`, `zoom.out`, `zoom.fit`, `zoom.selection` | WP3 (timeline) |
//! | `settings.open` | WP6 (Settings) |
//! | `export.panel`, `export.show`, `bin.preview` | WP7 (export panel, bin) |
//! | `autocut.page` | WP5 (panel) |
//! | `region.next`, `region.prev`, `app.quit` | WP0b (window, app) |
//! | `keys.help`, `palette.open` | WP0b: the "Not built yet" toast; WP9 re-registers them |
//!
//! `export.panel`, `autocut.page` and `bin.preview` toggle what they open:
//! the `Esc` chain runs them to close the passive export panel, the
//! autocut page (its Cancel) and a bin preview; `export.show` (a toast's
//! Show export) only opens the export panel. A later registration of an
//! id replaces the earlier one. An id nobody registered logs at debug
//! level.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// A registered action.
pub type Action = Rc<dyn Fn()>;

/// The registry. Cheap to clone; main thread only.
#[derive(Clone, Default)]
pub struct ActionRouter {
    actions: Rc<RefCell<BTreeMap<String, Action>>>,
}

impl std::fmt::Debug for ActionRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.actions.borrow().keys())
            .finish()
    }
}

impl ActionRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes `id` run `f` (replacing what ran before).
    pub fn register(&self, id: &str, f: Rc<dyn Fn()>) {
        self.actions.borrow_mut().insert(id.to_owned(), f);
    }

    /// Runs `id`; `false` when nobody registered it.
    pub fn run(&self, id: &str) -> bool {
        let action = self.actions.borrow().get(id).cloned();
        match action {
            Some(action) => {
                crate::guard::run("a GUI action", || action());
                true
            }
            None => {
                tracing::debug!("no GUI action {id} yet");
                false
            }
        }
    }

    /// Whether `id` is registered.
    pub fn is_registered(&self, id: &str) -> bool {
        self.actions.borrow().contains_key(id)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn registered_actions_run_and_can_be_replaced() {
        let router = ActionRouter::new();
        assert!(!router.run("zoom.in"));
        let count = Rc::new(Cell::new(0));
        let c = Rc::clone(&count);
        router.register("zoom.in", Rc::new(move || c.set(c.get() + 1)));
        assert!(router.run("zoom.in"));
        assert_eq!(count.get(), 1);
        let c = Rc::clone(&count);
        router.register("zoom.in", Rc::new(move || c.set(c.get() + 10)));
        router.run("zoom.in");
        assert_eq!(count.get(), 11);
        // An action may register others while it runs.
        let inner = router.clone();
        router.register(
            "palette.open",
            Rc::new(move || inner.register("keys.help", Rc::new(|| {}))),
        );
        assert!(router.run("palette.open"));
        assert!(router.is_registered("keys.help"));
    }
}
