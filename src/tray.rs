//! Tray mode. The window runs on the main thread and only starts work: every
//! action runs on a short-lived thread, so a slow network or a slow wallpaper
//! change never freezes the menu.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::thread;

use crate::app::{self, Ctx};
use crate::win::shell::{self, Action};
use crate::win::{self, Instance};

const THREAD_STACK: usize = 512 * 1024;

static CTX: OnceLock<Ctx> = OnceLock::new();
static SYNCING: AtomicBool = AtomicBool::new(false);

/// Run the tray until the user picks Quit. Returns at once if another tray runs.
pub fn run(ctx: Ctx) -> i32 {
    let _guard = match win::single_instance() {
        Instance::Running => return 0,
        Instance::First(g) => g,
    };
    let ctx = CTX.get_or_init(|| ctx);
    shell::run(&app::title(ctx), on_action)
}

fn on_action(action: Action) {
    match action {
        Action::Sync => start_sync(),
        Action::Next => quick(|c| drop(app::step(c, 1))),
        Action::Previous => quick(|c| drop(app::step(c, -1))),
        Action::Today => {
            // Show the newest image we have right now; the sync may bring a newer one.
            quick(|c| drop(app::show_newest(c)));
            start_sync();
        }
    }
}

fn spawn(job: impl FnOnce() + Send + 'static) -> bool {
    thread::Builder::new().stack_size(THREAD_STACK).spawn(job).is_ok()
}

fn refresh_tooltip(ctx: &Ctx) {
    shell::set_tooltip(&app::title(ctx));
}

/// Run a short action on its own thread, then refresh the tooltip.
/// Errors are dropped: the tray has nowhere to show them.
fn quick(job: fn(&Ctx)) {
    if let Some(ctx) = CTX.get() {
        spawn(move || {
            job(ctx);
            refresh_tooltip(ctx);
        });
    }
}

/// Check Bing on a thread. If a check is already running, do nothing.
fn start_sync() {
    let Some(ctx) = CTX.get() else { return };
    if SYNCING.swap(true, Ordering::AcqRel) {
        return;
    }
    let started = spawn(move || {
        // A network failure is not shown; the next timer tick tries again.
        let _ = app::sync(ctx);
        SYNCING.store(false, Ordering::Release);
        refresh_tooltip(ctx);
    });
    if !started {
        SYNCING.store(false, Ordering::Release);
    }
}
