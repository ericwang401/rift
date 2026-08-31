//! Puts a window back where it was after native fullscreen.
//!
//! Native fullscreen moves a window to a space of its own, and rift takes it
//! out of its tree; the siblings reflow. When it comes back, it used to be
//! appended after the selection, somewhere else, with the old split ratios
//! gone. This remembers the window's slot on the way out and reinstates it on
//! the way back:
//!
//! - the layout is snapshotted *before* the window leaves, and the tree as it
//!   looks *after* it has left is recorded as a digest;
//! - on exit, if the tree still matches that digest — nothing was touched
//!   while the window was away — the pre-departure layout is restored
//!   outright: same structure, nesting and ratios;
//! - if the tree was edited meanwhile, those edits are kept: the window is
//!   re-inserted next to its old neighbour on the same side with its old
//!   share, or, if that neighbour is gone too, wherever it would otherwise
//!   have landed.

use tracing::{debug, info, warn};

use super::{LayoutEvent, Reactor};
use crate::actor::app::WindowId;
use crate::common::collections::HashMap;
use crate::layout_engine::{RestoreRequest, RestoreScope, RestoreSource, Slot};
use crate::sys::screen::SpaceId;

#[derive(Default)]
pub(super) struct FullscreenSlots {
    slots: HashMap<WindowId, FullscreenSlot>,
}

struct FullscreenSlot {
    space: SpaceId,
    /// The engine as it was with the window still in its tree.
    snapshot: String,
    /// The tree with the window gone, before anything else happened to it.
    digest_after_removal: Option<String>,
    anchor: Option<Slot>,
}

impl FullscreenSlots {
    pub(super) fn forget(&mut self, window: WindowId) { self.slots.remove(&window); }
}

impl Reactor {
    /// Called from the layout-event sink just before the removal that takes
    /// a window entering native fullscreen out of its tree.
    pub(super) fn record_fullscreen_slot(&mut self, window: WindowId) {
        if self.state.windows.native_fullscreen_record_for_window(window).is_none() {
            return;
        }
        let Some(space) = self.assigned_space_for_window_id(window) else {
            return;
        };
        let engine = &mut self.layout_manager.layout_engine;
        if engine.is_window_floating(window) {
            // A float comes back as a float; its frame is kept elsewhere.
            return;
        }
        let anchor = engine.slot_of(space, window);
        let snapshot = match engine.snapshot_current_layout_lightly(&self.state.windows) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                debug!(?window, %error, "Could not snapshot the layout for a fullscreen slot");
                return;
            }
        };
        self.fullscreen_slots.slots.insert(window, FullscreenSlot {
            space,
            snapshot,
            digest_after_removal: None,
            anchor,
        });
    }

    /// Called right after that removal has been applied.
    pub(super) fn seal_fullscreen_slot(&mut self, window: WindowId) {
        let Some(slot) = self.fullscreen_slots.slots.get_mut(&window) else {
            return;
        };
        if slot.digest_after_removal.is_none() {
            slot.digest_after_removal = self.layout_manager.layout_engine.tree_digest(slot.space);
        }
    }

    /// The window is back on `space`. Reinstate its slot and add it to the
    /// layout; returns whether the active layout changed, like
    /// `restore_window_to_active_layout_if_visible`.
    pub(super) fn restore_window_to_layout_after_fullscreen(
        &mut self,
        window: WindowId,
        space: SpaceId,
    ) -> bool {
        let Some(slot) = self.fullscreen_slots.slots.remove(&window) else {
            return self.restore_window_to_active_layout_if_visible(window, space);
        };
        if slot.space != space {
            debug!(?window, "Fullscreen exit landed on another space; slot dropped");
            return self.restore_window_to_active_layout_if_visible(window, space);
        }

        let untouched = slot.digest_after_removal.is_some()
            && self.layout_manager.layout_engine.tree_digest(space) == slot.digest_after_removal;
        if untouched {
            let request = RestoreRequest {
                scope: RestoreScope::Workspace,
                active_space: space,
                source: RestoreSource::CurrentSpace,
            };
            let layout_settings = self.config.settings.layout.clone();
            match self.layout_manager.layout_engine.restore_layout_from_snapshot(
                &slot.snapshot,
                request,
                &mut self.state.windows,
                &layout_settings,
            ) {
                Ok(report) if report.matched > 0 => {
                    info!(
                        ?window,
                        matched = report.matched,
                        "Fullscreen exit: layout put back as it was"
                    );
                    // Idempotent for a window the restore already placed;
                    // this is what re-projects it if it is not visible yet.
                    let _ = self.restore_window_to_active_layout_if_visible(window, space);
                    return true;
                }
                Ok(report) => debug!(
                    ?window,
                    ?report,
                    "Fullscreen slot snapshot matched nothing; re-anchoring"
                ),
                Err(error) => {
                    warn!(?window, %error, "Fullscreen slot snapshot could not be restored; re-anchoring")
                }
            }
        }

        let changed = self.restore_window_to_active_layout_if_visible(window, space);
        if let Some(anchor) = slot.anchor
            && anchor.anchor != window
            && self.layout_manager.layout_engine.restore_slot(space, anchor, window)
        {
            info!(?window, anchor = ?anchor.anchor, side = ?anchor.side, "Fullscreen exit: window put back beside its old neighbour");
            return true;
        }
        changed
    }

    pub(super) fn note_fullscreen_slot_lifecycle(&mut self, event: &LayoutEvent) {
        match event {
            LayoutEvent::WindowRemovedPreserveFloating(window) => {
                self.record_fullscreen_slot(*window)
            }
            LayoutEvent::WindowRemoved(window) => self.fullscreen_slots.forget(*window),
            _ => {}
        }
    }
}
