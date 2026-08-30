use objc2_core_foundation::{CGPoint, CGRect};
use tracing::{trace, warn};

use crate::sys::geometry::CGRectExt;

use crate::actor::app::WindowId;
use crate::actor::reactor::events::EventOutcome;
use crate::actor::reactor::managers::{DragManager, LayoutManager};
use crate::actor::reactor::{DragState, LayoutEvent};
use crate::common::collections::HashMap;
use crate::layout_engine::LayoutCommand;
use crate::model::RiftState;
use crate::sys::screen::SpaceId;

#[derive(Debug, Clone)]
pub struct MouseUpPayload {
    pub pending_swap: Option<(WindowId, WindowId)>,
    /// What the drop means, from where the cursor sits inside the target.
    /// `None` falls back to swapping, which is what a drop always used to do.
    pub drop_action: Option<crate::actor::drag_swap::DropAction>,
    pub swap_space: Option<SpaceId>,
    pub final_space: Option<SpaceId>,
    pub visible_spaces: Vec<SpaceId>,
    pub visible_space_centers: HashMap<SpaceId, CGPoint>,
    /// Every display's frame, for judging whether a drop straddles the seam.
    pub screens: Vec<CGRect>,
    /// Where the pointer let go.
    pub pointer: Option<CGPoint>,
}

pub fn handle_mouse_up(
    state: &mut RiftState,
    layout: &mut LayoutManager,
    drag: &mut DragManager,
    payload: MouseUpPayload,
) -> anyhow::Result<EventOutcome> {
    let mut outcome = EventOutcome::layout_changed(false);
    let mut needs_layout = false;

    let mut moved_by_drop = false;
    if let Some((dragged, target)) = payload.pending_swap {
        drag.skip_layout_for_window = Some(dragged);
        if state.windows.contains_window(dragged) && state.windows.contains_window(target) {
            // Dropping on the middle of a window exchanges the two; dropping
            // near an edge splits the target and puts the dragged window on
            // that side. A layout that cannot express the split says so, and
            // the drop falls back to the swap it would have been before.
            let mut handled = false;
            if let (Some(crate::actor::drag_swap::DropAction::Insert(direction)), Some(space)) =
                (payload.drop_action, payload.swap_space)
            {
                // A drop from another display: the dragged window's tree
                // membership was frozen for the drag, so it still lives on
                // its origin display. Move it — out of every tree, onto the
                // target's space and workspace — before the split, so the
                // membership the insert creates is the only one it ends up
                // with.
                if !layout.layout_engine.is_window_tiled(space, dragged) {
                    let removal = layout
                        .layout_engine
                        .handle_event(&mut state.windows, LayoutEvent::WindowRemoved(dragged));
                    outcome = outcome.with_layout_response(removal.response, None);
                    if let Some(server_id) =
                        state.windows.window(dragged).and_then(|window| window.info.sys_id)
                    {
                        state.windows.set_window_server_space(server_id, Some(space));
                        state.windows.mark_window_visible(server_id);
                    }
                    if let Some(workspace) = layout.layout_engine.active_workspace(space)
                        && !layout
                            .layout_engine
                            .virtual_workspace_manager_mut()
                            .assign_window_to_workspace(&mut state.windows, space, dragged, workspace)
                    {
                        warn!(?dragged, ?workspace, "failed to assign dragged window");
                    }
                    moved_by_drop = true;
                }
                trace!(?dragged, ?target, ?direction, "inserting beside the drop target");
                handled =
                    layout.layout_engine.insert_window_next_to(space, target, direction, dragged);
                if moved_by_drop && !handled {
                    // Already out of its old tree; a plain add on the target's
                    // space is the honest fallback, not a swap the window can
                    // no longer take part in.
                    outcome = outcome.with_layout_event(LayoutEvent::WindowAdded(space, dragged));
                    handled = true;
                }
            }
            if !handled {
                trace!(?dragged, ?target, "performing deferred drag swap");
                let response = layout.layout_engine.handle_command(
                    &mut state.windows,
                    payload.swap_space,
                    &payload.visible_spaces,
                    &payload.visible_space_centers,
                    LayoutCommand::SwapWindows(dragged.into(), target.into()),
                );
                outcome = outcome.with_layout_response(response, None);
            }
        }
        needs_layout = true;
    }

    let session = match std::mem::replace(&mut drag.drag_state, DragState::Inactive) {
        DragState::Active { session } | DragState::PendingSwap { session, .. } => Some(session),
        DragState::Inactive => None,
    };
    if let Some(session) = session {
        let window = session.window;
        if moved_by_drop && Some(window) == payload.pending_swap.map(|(dragged, _)| dragged) {
            // The drop itself moved the window between trees; only the
            // arrange is still owed.
            drag.skip_layout_for_window = Some(window);
            needs_layout = true;
        } else if session.origin_space != payload.final_space {
            if session.origin_space.is_some() {
                // A float dragged onto another space is still a float. Plain
                // `WindowRemoved` drops the floating mark, and the
                // `WindowAdded` below then tiled it — a floating window that
                // was merely dragged across displays landed in the tree.
                let removal = if layout.layout_engine.is_window_floating(window) {
                    LayoutEvent::WindowRemovedPreserveFloating(window)
                } else {
                    LayoutEvent::WindowRemoved(window)
                };
                outcome = outcome.with_layout_event(removal);
            }
            if let Some(space) = payload.final_space {
                if let Some(server_id) =
                    state.windows.window(window).and_then(|window| window.info.sys_id)
                {
                    state.windows.set_window_server_space(server_id, Some(space));
                    state.windows.mark_window_visible(server_id);
                }
                if let Some(workspace) = layout.layout_engine.active_workspace(space)
                    && !layout
                        .layout_engine
                        .virtual_workspace_manager_mut()
                        .assign_window_to_workspace(&mut state.windows, space, window, workspace)
                {
                    warn!(?window, ?workspace, "failed to assign dragged window");
                }
                outcome = outcome.with_layout_event(LayoutEvent::WindowAdded(space, window));
            }
            drag.skip_layout_for_window = Some(window);
            needs_layout = true;
        } else if session.layout_dirty {
            drag.skip_layout_for_window = Some(window);
            needs_layout = true;
        }

        if let Some(space) = payload.final_space
            && layout.layout_engine.is_window_floating(window)
        {
            if session.origin_space != payload.final_space {
                layout.layout_engine.remove_floating_position(window);
            }
            if let Some(workspace) = layout
                .layout_engine
                .virtual_workspace_manager()
                .workspace_for_window(&state.windows, space, window)
                .or_else(|| layout.layout_engine.active_workspace(space))
            {
                // Where the window server has the window now, not where the
                // session last saw it: the app's frame reports for this drag
                // may all have been discarded (they trail rift's own last
                // write), leaving `last_frame` at some earlier drag's end.
                // Storing that made the next arrange put the float back
                // there — the float "jumping" on release.
                let dropped_at = state
                    .windows
                    .window(window)
                    .and_then(|w| w.info.sys_id)
                    .and_then(crate::sys::window_server::live_window_frame)
                    .filter(|frame| frame.size.width > 0.0 && frame.size.height > 0.0)
                    .unwrap_or(session.last_frame);
                // macOS does not let a *programmatically* moved window rest
                // straddling the display seam ("Displays have Separate
                // Spaces": a window belongs to one display). An app that
                // animates its own drag (Warp's tab bar) drops the window
                // wherever the hand let go — sometimes straddling — and the
                // system then relocates it to a display of its own choosing:
                // the float "snapping" on release. Finish the user's intent
                // instead: the display under the pointer keeps the window,
                // shifted just enough to fit on it.
                // A straddling drop is watched, not corrected: macOS lets a
                // server-side drag rest overhanging the seam (clipped), and
                // preemptively "finishing" such a drop stole a legitimate
                // resting place. Only if the system or the app relocates the
                // window away from the drop does `assert_seam_finish` step
                // in with a deterministic placement.
                if seam_fitted(&payload.screens, payload.pointer, dropped_at).is_some() {
                    trace!(?window, ?dropped_at, "watching a seam-straddling drop");
                    drag.seam_finish = Some(crate::actor::reactor::managers::SeamFinish {
                        window,
                        dropped_at,
                        pointer: payload.pointer,
                        fitted: None,
                        at: crate::sys::trace::now(),
                        attempts: 0,
                    });
                }
                // A drop is an observation, not an intent: the window is
                // already where the user put it. `store_floating_position`
                // plants a `pending_float_placement`, and when no arrange
                // ran before the user's *next* drag (mid-drag arranges skip
                // the held window), the drop-arrange asserted the previous
                // drop's stored frame over the live one — the float
                // snapping back on release.
                layout
                    .layout_engine
                    .follow_floating_position(space, workspace, window, dropped_at);
            }
        }
    }

    drag.reset();
    drag.drag_state = DragState::Inactive;
    let skipped = drag.skip_layout_for_window.is_some();
    drag.skip_layout_for_window = None;

    // Having skipped layout for a window during the drag is itself the reason
    // to run one now: its frame was left to follow the pointer and no longer
    // matches the tree. Without this the window simply stays wherever it was
    // dropped, which happens whenever the drag ends with no swap to perform —
    // the session's own dirty flag stops being updated once a swap candidate
    // promotes the drag out of the Active state.
    let needs_layout = needs_layout || skipped;

    let passes = if needs_layout {
        if skipped { 3 } else { 2 }
    } else {
        0
    };
    Ok(outcome.with_arrange_passes(passes))
}

/// Where a frame that would rest straddling the display seam should be
/// finished instead: fully clear of every display but the one under the
/// pointer (falling back to the display holding most of the frame), nudged
/// the shortest distance that gets it there. `None` when the frame already
/// rests on a single display — hanging off an edge into empty space is
/// legal, only overlapping a second display is not (macOS relocates such a
/// window to a display of its own choosing; see the seam-drop notes).
pub(crate) fn seam_fitted(
    screens: &[CGRect],
    pointer: Option<CGPoint>,
    frame: CGRect,
) -> Option<CGRect> {
    let overlap_area = |screen: &CGRect, frame: &CGRect| {
        let i = screen.intersection(frame);
        if i.size.width > 1.0 && i.size.height > 1.0 {
            i.size.width * i.size.height
        } else {
            0.0
        }
    };
    let landing = pointer
        .and_then(|p| screens.iter().copied().find(|s| s.contains(p)))
        .or_else(|| {
            screens
                .iter()
                .copied()
                .max_by(|a, b| overlap_area(a, &frame).total_cmp(&overlap_area(b, &frame)))
                .filter(|screen| overlap_area(screen, &frame) > 0.0)
        })?;
    let straddles = screens
        .iter()
        .any(|other| *other != landing && overlap_area(other, &frame) > 0.0);
    if !straddles {
        return None;
    }
    let mut fitted = frame;
    for other in screens {
        if *other == landing || overlap_area(other, &fitted) == 0.0 {
            continue;
        }
        // Push targets land inside the landing display's span on that axis,
        // not merely past the other display's edge: display frames can have
        // a gap between them, and a frame left in the gap is still
        // relocated.
        let targets = [
            ((other.origin.x + other.size.width).max(landing.origin.x), true),
            (
                other.origin.x.min(landing.origin.x + landing.size.width) - fitted.size.width,
                true,
            ),
            ((other.origin.y + other.size.height).max(landing.origin.y), false),
            (
                other.origin.y.min(landing.origin.y + landing.size.height) - fitted.size.height,
                false,
            ),
        ];
        if let Some(&(target, horizontal)) = targets.iter().min_by(|(a, ah), (b, bh)| {
            let da = (a - if *ah { fitted.origin.x } else { fitted.origin.y }).abs();
            let db = (b - if *bh { fitted.origin.x } else { fitted.origin.y }).abs();
            da.total_cmp(&db)
        }) {
            if horizontal {
                fitted.origin.x = target;
            } else {
                fitted.origin.y = target;
            }
        }
    }
    Some(fitted)
}

