use objc2_core_foundation::CGPoint;
use tracing::{trace, warn};

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
}

pub fn handle_mouse_up(
    state: &mut RiftState,
    layout: &mut LayoutManager,
    drag: &mut DragManager,
    payload: MouseUpPayload,
) -> anyhow::Result<EventOutcome> {
    let mut outcome = EventOutcome::layout_changed(false);
    let mut needs_layout = false;

    if let Some((dragged, target)) = payload.pending_swap {
        drag.skip_layout_for_window = Some(dragged);
        if state.windows.contains_window(dragged) && state.windows.contains_window(target) {
            // Dropping on the middle of a window exchanges the two; dropping
            // near an edge splits the target and puts the dragged window on
            // that side. A layout that cannot express the split says so, and
            // the drop falls back to the swap it would have been before.
            let inserted = match (payload.drop_action, payload.swap_space) {
                (Some(crate::actor::drag_swap::DropAction::Insert(direction)), Some(space)) => {
                    trace!(?dragged, ?target, ?direction, "inserting beside the drop target");
                    layout.layout_engine.insert_window_next_to(space, target, direction, dragged)
                }
                _ => false,
            };
            if !inserted {
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
        if session.origin_space != payload.final_space {
            if session.origin_space.is_some() {
                outcome = outcome.with_layout_event(LayoutEvent::WindowRemoved(window));
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
                layout.layout_engine.store_floating_position(
                    space,
                    workspace,
                    window,
                    session.last_frame,
                );
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
    needs_layout |= skipped;

    let passes = if needs_layout {
        if skipped { 3 } else { 2 }
    } else {
        0
    };
    Ok(outcome.with_arrange_passes(passes))
}
