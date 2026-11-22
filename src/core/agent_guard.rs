//! Agent guard: keep screen-reading / input-driving automation (AI "computer use" agents,
//! macro tools) away from the Ruxt windows.
//!
//! - **Capture exclusion.** Every Ruxt viewport is marked content-protected
//!   (`ViewportCommand::ContentProtected`, i.e. `WDA_EXCLUDEFROMCAPTURE` on Windows), so
//!   screenshots, recordings and screen sharing do not show it.
//! - **Injected-input block.** Low-level hooks (crate `ruxt-inputguard`, the only FFI) drop
//!   keyboard / mouse input that a program injected while it is aimed at a Ruxt window; the
//!   app turns that into a screen lock.
//!
//! This is a guard against casual automation, not a security boundary: a program running as
//! the same OS user can still read this process's memory and files.

use std::time::{Duration, Instant};

/// Whether this platform implements the guard (the input hooks are Windows-only).
pub const SUPPORTED: bool = cfg!(windows);

/// How often content protection is re-sent, so newly opened viewports get it too.
const REAPPLY_EVERY: Duration = Duration::from_secs(1);

/// True once after the hooks dropped injected input aimed at a Ruxt window.
pub fn take_tripped() -> bool {
    ruxt_inputguard::take_tripped()
}

#[derive(Default)]
pub struct AgentGuard {
    on: bool,
    last_apply: Option<Instant>,
    hooks: Option<ruxt_inputguard::Hooks>,
}

impl AgentGuard {
    /// Bring the guard in line with `want` (call every frame with the config value).
    pub fn sync(&mut self, want: bool, ctx: &egui::Context) {
        let want = want && SUPPORTED;
        if want != self.on {
            self.on = want;
            self.last_apply = None;
            if want {
                let repaint = ctx.clone();
                ruxt_inputguard::set_on_trip(move || repaint.request_repaint());
                self.hooks = ruxt_inputguard::Hooks::start();
                if self.hooks.is_none() {
                    tracing::warn!("agent guard: installing input hooks failed");
                }
            } else {
                self.hooks = None; // Dropping unhooks.
                protect_all(ctx, false);
            }
        }
        if self.on && self.last_apply.is_none_or(|t| t.elapsed() >= REAPPLY_EVERY) {
            self.last_apply = Some(Instant::now());
            protect_all(ctx, true);
            ctx.request_repaint_after(REAPPLY_EVERY);
        }
    }
}

/// Mark every open viewport (main window and e.g. tail-viewer windows) content-protected.
fn protect_all(ctx: &egui::Context, on: bool) {
    let ids: Vec<egui::ViewportId> = ctx.input(|i| i.raw.viewports.keys().copied().collect());
    for id in ids {
        ctx.send_viewport_cmd_to(id, egui::ViewportCommand::ContentProtected(on));
    }
}
