//! Keeps a display's layout while the display is away, and puts it back when
//! the display returns.
//!
//! macOS does two things when a display disconnects — unplug, sleep, lid close,
//! a mode change that re-enumerates it — that between them lose a layout
//! entirely:
//!
//! 1. The display's space is destroyed, and the display gets a *brand-new*
//!    space id when it comes back. Layouts are keyed by space id, so the old
//!    tree is orphaned and the new space starts from nothing.
//! 2. Every window on it is moved to whichever display survived, and nothing
//!    ever moves them back.
//!
//! The archive answers both. When a display departs, its layout is snapshotted
//! (the same serialization the master file uses, kept in memory) together with
//! the windows that were on it; those windows are then left to land on the
//! surviving display as floats (or, with `displaced_windows = "tile"`, as one
//! cluster in that tree). When a display with the same UUID reappears, the old
//! space's workspaces are remapped onto the new space id, the exiled windows
//! are moved home through the scripting addition, and once they have landed
//! the snapshot is restored onto the new space. Anything that never lands —
//! closed in the meantime, or no scripting addition — is simply left out.

use std::time::{Duration, Instant};

use dispatchr::queue;
use dispatchr::time::Time;
use objc2_core_foundation::CGSize;
use tracing::{debug, info, warn};

use super::{Event, LayoutEvent, Reactor};
use crate::actor::app::WindowId;
use crate::actor::reactor::events::EventOutcome;
use crate::common::collections::{HashMap, HashSet};
use crate::common::config::DisplacedWindows;
use crate::layout_engine::{RestoreRequest, RestoreScope, RestoreSource};
use crate::sys::dispatch::DispatchExt;
use crate::sys::screen::ScreenInfo;
use crate::sys::screen::SpaceId;
use crate::sys::scripting_addition;
use crate::sys::window_server::WindowServerId;

/// How long to wait for exiled windows to arrive on the returned display
/// before restoring the layout with whatever has landed.
const HOMING_DEADLINE: Duration = Duration::from_secs(3);

/// How long a pre-churn snapshot stays usable. A display change is seen
/// within a second or two of the window server starting to move windows.
const PRE_CHURN_TTL: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(super) struct DisplayArchive {
    entries: HashMap<String, ArchivedDisplay>,
    /// The layout as it was just before the window server started moving
    /// windows between spaces on its own. See `capture_pre_churn_layout`.
    pre_churn: Option<PreChurn>,
}

/// The window server starts moving windows the moment a display goes, and
/// its reports (and the AX frame notifications that follow) reach the reactor
/// before the display change itself does: by the time a display is seen to
/// have departed, rift has already re-homed some of its windows — and some
/// of the *survivor's* windows, which macOS merges into the departed
/// display's space — and pulled them out of their trees. So the first
/// cross-space removal that no drag and no rift move accounts for is taken
/// as the start of a churn, and the layout is snapshotted before it happens.
/// If a display change follows within the TTL, that snapshot is the one
/// worth keeping.
struct PreChurn {
    layout: String,
    /// The windows of every space, in layout order, as the trees had them.
    members: HashMap<SpaceId, Vec<WindowId>>,
    taken: Instant,
}

pub(super) struct ArchivedDisplay {
    /// The space the display showed when it departed. Kept current through
    /// remaps so the return trip knows which workspaces to move.
    space: SpaceId,
    /// `LayoutEngine::snapshot_current_layout` at departure, before any of the
    /// display's windows were touched.
    snapshot: String,
    /// The windows that were on the display, in layout order.
    windows: Vec<ExiledWindow>,
    /// Set once the displaced windows have been arranged on the surviving
    /// display, so the arrangement is not redone on every later snapshot.
    settled: bool,
    /// Where the windows were sent when another display took over this
    /// display's space. See `evict_from_taken_over_space`.
    parked_on: Option<SpaceId>,
    /// A survivor whose own space macOS destroyed when it inherited a departed
    /// display's: its layout waits here until that display is back, because
    /// only then does the survivor get a space of its own again.
    waiting_for: Option<String>,
    homing: Option<Homing>,
}

struct ExiledWindow {
    wid: WindowId,
    wsid: Option<WindowServerId>,
    /// The user's own float/tile choice before it was overridden to float.
    user_floating: Option<bool>,
    was_tiled: bool,
}

struct Homing {
    space: SpaceId,
    /// Windows asked to move home that the window server has not yet reported
    /// on the new space.
    waiting: HashSet<WindowId>,
    started: Instant,
}

impl DisplayArchive {
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn has(&self, display_uuid: &str) -> bool {
        self.entries.contains_key(display_uuid)
    }

    pub(super) fn is_homing(&self, display_uuid: &str) -> bool {
        self.entries.get(display_uuid).is_some_and(|entry| entry.homing.is_some())
    }

    /// A stay-behind archive is only actionable once the display it waits
    /// for is back on screen.
    fn is_ready(&self, display_uuid: &str, screens: &[ScreenInfo]) -> bool {
        self.entries
            .get(display_uuid)
            .and_then(|entry| entry.waiting_for.as_deref())
            .is_none_or(|waited| screens.iter().any(|screen| screen.display_uuid == waited))
    }

    fn fresh_pre_churn(&self) -> Option<&PreChurn> {
        self.pre_churn.as_ref().filter(|pre| pre.taken.elapsed() < PRE_CHURN_TTL)
    }

    fn any_homing(&self) -> bool {
        self.entries.values().any(|entry| entry.homing.is_some())
    }
}

/// Fallback `cgdisplay-N` ids are not identities: N is reassigned on every
/// reconnect, so a layout filed under one could never be found again.
fn is_stable_display_uuid(uuid: &str) -> bool {
    !uuid.starts_with("cgdisplay-")
}

impl Reactor {
    fn display_archive_enabled(&self) -> bool {
        self.config.settings.restore_display_layouts
    }

    /// An explicit command or drop on a displaced window while its display is
    /// away is a claim on it for the display it is on: it is no longer sent
    /// back on replug, and the display it is on takes it along wherever that
    /// display's own windows go. Automatic changes — macOS moving frames,
    /// rift reflowing — never adopt anything; only this is called for them.
    pub(super) fn adopt_displaced_window(&mut self, wid: WindowId) {
        let Some(from) = self
            .display_archive
            .entries
            .iter()
            .find(|(_, entry)| {
                entry.homing.is_none() && entry.windows.iter().any(|window| window.wid == wid)
            })
            .map(|(uuid, _)| uuid.clone())
        else {
            return;
        };
        let shown_on = self.assigned_space_for_window_id(wid).and_then(|space| {
            self.space_state
                .screens
                .iter()
                .find(|screen| screen.space == Some(space))
                .map(|screen| screen.display_uuid.clone())
        });
        if shown_on.as_deref() == Some(from.as_str()) {
            // Its own display is back on screen; nothing to claim.
            return;
        }
        let Some(state) = self.state.windows.window(wid) else {
            return;
        };
        let adopted = ExiledWindow {
            wid,
            wsid: state.info.sys_id,
            user_floating: self.state.windows.user_floating(wid),
            was_tiled: !self.layout_manager.layout_engine.is_window_floating(wid),
        };
        if let Some(entry) = self.display_archive.entries.get_mut(&from) {
            entry.windows.retain(|window| window.wid != wid);
        }
        // A display whose own space is gone takes the window along when it
        // gets a new one; a display whose space is alive simply keeps it.
        if let Some(uuid) = shown_on.as_deref()
            && let Some(entry) = self.display_archive.entries.get_mut(uuid)
        {
            entry.windows.retain(|window| window.wid != wid);
            entry.windows.push(adopted);
        }
        info!(?wid, from = %from, to = ?shown_on, "Displaced window adopted by the display it was handled on");
    }

    /// Called from the layout-event sink just before a window leaves its
    /// tree for another space without a drag. See `PreChurn`.
    pub(super) fn capture_pre_churn_layout(&mut self) {
        if !self.display_archive_enabled()
            || matches!(
                self.drag_manager.drag_state,
                super::DragState::Active { .. } | super::DragState::PendingSwap { .. }
            )
            || self.display_archive.fresh_pre_churn().is_some()
        {
            return;
        }
        let engine = &mut self.layout_manager.layout_engine;
        let members = engine
            .virtual_workspace_manager()
            .initialized_spaces()
            .into_iter()
            .map(|space| (space, engine.windows_on_space_in_layout_order(space)))
            .collect();
        match engine.snapshot_current_layout_lightly(&self.state.windows) {
            Ok(layout) => {
                self.display_archive.pre_churn = Some(PreChurn {
                    layout,
                    members,
                    taken: Instant::now(),
                });
            }
            Err(error) => debug!(%error, "Could not take a pre-churn layout snapshot"),
        }
    }

    /// The windows on `space`: those in its trees and floats now, plus any
    /// the last authoritative snapshot placed there that the churn has
    /// already moved away. Tree order first.
    fn windows_belonging_to_space(&self, space: SpaceId) -> Vec<ExiledWindow> {
        let engine = &self.layout_manager.layout_engine;
        let mut wids = engine.windows_on_space_in_layout_order(space);
        if let Some(pre) = self.display_archive.fresh_pre_churn()
            && let Some(members) = pre.members.get(&space)
        {
            let carried: Vec<WindowId> =
                members.iter().copied().filter(|wid| !wids.contains(wid)).collect();
            wids.extend(carried);
        }
        wids.into_iter()
            .filter_map(|wid| {
                let state = self.state.windows.window(wid)?;
                Some(ExiledWindow {
                    wid,
                    wsid: state.info.sys_id,
                    user_floating: self.state.windows.user_floating(wid),
                    was_tiled: !engine.is_window_floating(wid),
                })
            })
            .collect()
    }

    /// The layout to archive for a space that is going away: the pre-churn
    /// snapshot if there is a fresh one, else the layout as it is now.
    fn snapshot_for_departure(&mut self, space: SpaceId) -> anyhow::Result<String> {
        if let Some(pre) = self.display_archive.fresh_pre_churn() {
            return Ok(pre.layout.clone());
        }
        self.layout_manager
            .layout_engine
            .snapshot_current_layout(&self.state.windows, Some(space))
    }

    /// Called with the new display set before the engine forgets the displays
    /// missing from it. Archives each departed display's layout and readies
    /// its windows for life on the surviving display.
    pub(super) fn archive_departed_displays(
        &mut self,
        active_displays: &[String],
        screens: &[ScreenInfo],
        display_space_ids: &HashMap<String, Vec<SpaceId>>,
    ) {
        if !self.display_archive_enabled() {
            return;
        }
        let departed = self.layout_manager.layout_engine.departed_displays(active_displays);
        for (uuid, space) in departed {
            if !is_stable_display_uuid(&uuid) {
                continue;
            }
            // macOS does not destroy the *main* display's space when that
            // display goes: it migrates the space, layout and all, onto a
            // surviving display, and parks that display's own space behind
            // it. Nothing has moved, so nothing is displaced by the
            // reassignment below; the eviction in `archive_display` is what
            // handles it.
            let taken_over_by = screens
                .iter()
                .find(|screen| screen.space == Some(space))
                .map(|screen| screen.display_uuid.clone());
            if let Some(survivor) = &taken_over_by {
                self.archive_survivors_lost_space(survivor, &uuid, display_space_ids);
            }
            self.archive_display(uuid, space, taken_over_by, display_space_ids);
        }
        // Whatever was captured has served its purpose; the next churn gets
        // its own.
        self.display_archive.pre_churn = None;
    }

    fn archive_display(
        &mut self,
        uuid: String,
        space: SpaceId,
        taken_over_by: Option<String>,
        display_space_ids: &HashMap<String, Vec<SpaceId>>,
    ) {
        let windows = self.windows_belonging_to_space(space);

        if let Some(existing) = self.display_archive.entries.get_mut(&uuid) {
            // The display left again mid-return. The snapshot from its first
            // departure is the layout the user actually built; what is on the
            // new space now is at best a partial restore of it. Keep the old
            // snapshot, but track the space it now lives under and any windows
            // that did make it home so they are brought back next time too.
            existing.space = space;
            existing.homing = None;
            existing.settled = false;
            existing.parked_on = None;
            existing.waiting_for = None;
            for window in windows {
                if !existing.windows.iter().any(|known| known.wid == window.wid) {
                    existing.windows.push(window);
                }
            }
            match taken_over_by {
                None => self.displace_archived_windows(&uuid),
                Some(survivor) => {
                    self.evict_from_taken_over_space(&uuid, space, &survivor, display_space_ids)
                }
            }
            return;
        }

        let snapshot = match self.snapshot_for_departure(space) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                warn!(display = %uuid, space = space.get(), %error, "Could not snapshot the display's layout; it will not be restored");
                return;
            }
        };
        info!(
            display = %uuid,
            space = space.get(),
            windows = ?windows.iter().map(|window| (window.wid, window.was_tiled)).collect::<Vec<_>>(),
            "Display departed; archived its layout"
        );
        self.display_archive.entries.insert(
            uuid.clone(),
            ArchivedDisplay {
                space,
                snapshot,
                windows,
                settled: false,
                parked_on: None,
                waiting_for: None,
                homing: None,
            },
        );
        match taken_over_by {
            None => self.displace_archived_windows(&uuid),
            Some(survivor) => {
                self.evict_from_taken_over_space(&uuid, space, &survivor, display_space_ids)
            }
        }
    }

    /// When the survivor inherits the departed display's space, macOS may
    /// destroy the survivor's own space outright and merge its windows into
    /// the inherited one — so they would ride back to the other display on
    /// replug. Archive the survivor's layout under the survivor's own UUID,
    /// to be restored onto whatever space it is given once the departed
    /// display has taken its space back.
    fn archive_survivors_lost_space(
        &mut self,
        survivor: &str,
        departed: &str,
        new_display_space_ids: &HashMap<String, Vec<SpaceId>>,
    ) {
        let Some(lost) = self
            .space_state
            .screens
            .iter()
            .find(|screen| screen.display_uuid == survivor)
            .and_then(|screen| screen.space)
        else {
            return;
        };
        if new_display_space_ids.values().flatten().any(|space| *space == lost)
            || self.display_archive.entries.contains_key(survivor)
        {
            return;
        }
        let windows = self.windows_belonging_to_space(lost);
        let snapshot = match self.snapshot_for_departure(lost) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                warn!(%survivor, space = lost.get(), %error, "Could not snapshot the survivor's layout");
                return;
            }
        };
        info!(
            %survivor,
            %departed,
            lost_space = lost.get(),
            windows = ?windows.iter().map(|window| window.wid).collect::<Vec<_>>(),
            "Survivor's own space was destroyed by the takeover; archived its layout until the other display is back"
        );
        self.display_archive.entries.insert(
            survivor.to_string(),
            ArchivedDisplay {
                space: lost,
                snapshot,
                windows,
                settled: true,
                parked_on: None,
                waiting_for: Some(departed.to_string()),
                homing: None,
            },
        );
    }

    /// The space a surviving display should go back to when it has been
    /// handed a departed display's space: one of its own, never one that
    /// came along from the departed display. macOS moves *all* of the
    /// departed display's desktops to the survivor, and by the time the
    /// takeover is seen the survivor's "last user space" may already be the
    /// one it was handed, so the departed display's own space list (from the
    /// snapshot before this one) is what tells the two apart. The survivor's
    /// previous user space is preferred; failing that, any space that was
    /// its own.
    pub(super) fn takeover_parking_space(
        &self,
        survivor: &str,
        departed: &str,
        taken_over: SpaceId,
        display_space_ids: &HashMap<String, Vec<SpaceId>>,
    ) -> Option<SpaceId> {
        let departed_spaces =
            self.space_state.display_space_ids.get(departed).cloned().unwrap_or_default();
        let own: Vec<SpaceId> = display_space_ids
            .get(survivor)?
            .iter()
            .copied()
            .filter(|space| *space != taken_over && !departed_spaces.contains(space))
            .collect();
        self.space_state
            .last_user_space_by_display
            .get(survivor)
            .copied()
            .filter(|space| own.contains(space))
            .or_else(|| own.first().copied())
    }

    /// Another display has inherited the departed display's space, tree and
    /// all, squeezed onto its screen — the one thing the archive exists to
    /// prevent. Send the departed display's windows to the survivor's own
    /// space as floats and switch the survivor back to it, so it shows the
    /// layout it had with the visitors floating over it. Both moves need the
    /// scripting addition; without it the takeover is left as macOS made it.
    fn evict_from_taken_over_space(
        &mut self,
        uuid: &str,
        taken_over: SpaceId,
        survivor: &str,
        display_space_ids: &HashMap<String, Vec<SpaceId>>,
    ) {
        if self.config.settings.displaced_windows != DisplacedWindows::Float {
            return;
        }
        let Some(parking) =
            self.takeover_parking_space(survivor, uuid, taken_over, display_space_ids)
        else {
            debug!(display = %uuid, %survivor, "Space taken over, but the survivor has no space of its own to go back to");
            return;
        };
        if !scripting_addition::is_available() {
            warn!(
                display = %uuid,
                %survivor,
                "Space taken over by another display; moving its windows aside needs the scripting addition"
            );
            return;
        }
        let Some(entry) = self.display_archive.entries.get_mut(uuid) else {
            return;
        };
        let mut moved = 0usize;
        for window in &entry.windows {
            let Some(wsid) = window.wsid else {
                continue;
            };
            if scripting_addition::move_window_to_space(wsid.as_u32(), parking.get()) {
                moved += 1;
            }
        }
        entry.parked_on = Some(parking);
        info!(
            display = %uuid,
            %survivor,
            taken_over = taken_over.get(),
            parking = parking.get(),
            moved,
            "Space taken over by another display; parked its windows on the survivor's own space"
        );
        self.displace_archived_windows(uuid);
        if !scripting_addition::focus_space(parking.get()) {
            warn!(
                parking = parking.get(),
                "Could not switch the surviving display back to its own space"
            );
        }
    }

    /// Prepare a departed display's windows for the surviving display. In
    /// float mode this happens before they are reassigned: flagging them
    /// floating now means the reassignment projects them as floats, and the
    /// user-level override keeps app rules from tiling them back.
    fn displace_archived_windows(&mut self, uuid: &str) {
        if self.config.settings.displaced_windows != DisplacedWindows::Float {
            return;
        }
        let Some(entry) = self.display_archive.entries.get(uuid) else {
            return;
        };
        let tiled: Vec<WindowId> = entry
            .windows
            .iter()
            .filter(|window| window.was_tiled)
            .map(|window| window.wid)
            .collect();
        for wid in tiled {
            self.state.windows.set_user_floating(wid, true);
            self.layout_manager.layout_engine.mark_window_floating(wid);
        }
    }

    /// After a snapshot has been reconciled: in tile mode, gather each
    /// departed display's windows into one cluster of the tree they landed
    /// in, once they have all landed.
    pub(super) fn settle_displaced_windows(&mut self) {
        if self.display_archive.is_empty()
            || self.config.settings.displaced_windows != DisplacedWindows::Tile
        {
            return;
        }
        let uuids: Vec<String> = self
            .display_archive
            .entries
            .iter()
            .filter(|(_, entry)| !entry.settled && entry.homing.is_none())
            .map(|(uuid, _)| uuid.clone())
            .collect();
        for uuid in uuids {
            let entry = &self.display_archive.entries[&uuid];
            let tiled: Vec<WindowId> = entry
                .windows
                .iter()
                .filter(|window| window.was_tiled)
                .map(|window| window.wid)
                .filter(|wid| self.state.windows.window(*wid).is_some())
                .collect();
            let destinations: HashSet<SpaceId> =
                tiled.iter().filter_map(|wid| self.assigned_space_for_window_id(*wid)).collect();
            if destinations.contains(&entry.space) {
                // Not everything has been reassigned off the dead space yet.
                continue;
            }
            for space in destinations {
                self.layout_manager.layout_engine.cluster_windows_after_selection(space, &tiled);
            }
            if let Some(entry) = self.display_archive.entries.get_mut(&uuid) {
                entry.settled = true;
            }
        }
    }

    /// Called once the current screens are known and the engine's
    /// display→space map is up to date. Starts the return trip for every
    /// archived display that is on screen again.
    pub(super) fn begin_display_homing(&mut self) -> EventOutcome {
        let mut outcome = EventOutcome::default();
        if self.display_archive.is_empty() {
            return outcome;
        }
        let returned: Vec<(String, SpaceId)> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| Some((screen.display_uuid.clone(), screen.space?)))
            .filter(|(uuid, _)| {
                self.display_archive.has(uuid)
                    && !self.display_archive.is_homing(uuid)
                    && self.display_archive.is_ready(uuid, &self.space_state.screens)
            })
            .collect();
        for (uuid, shown) in returned {
            // A display with several desktops can come back showing a
            // different one than it left on. The layout belongs on the desktop
            // it was built on, so if that space still exists on the display
            // the windows go there and the display is switched to it.
            let target = self.homing_target(&uuid, shown);
            outcome.absorb(self.home_display(&uuid, target));
            if target != shown
                && self.display_archive.is_homing(&uuid)
                && !scripting_addition::focus_space(target.get())
            {
                warn!(display = %uuid, space = target.get(), "Could not switch the returned display to its restored desktop");
            }
        }
        outcome
    }

    pub(super) fn homing_target(&self, uuid: &str, shown: SpaceId) -> SpaceId {
        let Some(entry) = self.display_archive.entries.get(uuid) else {
            return shown;
        };
        let still_there = self
            .space_state
            .display_space_ids
            .get(uuid)
            .is_some_and(|spaces| spaces.contains(&entry.space));
        if still_there { entry.space } else { shown }
    }

    fn home_display(&mut self, uuid: &str, new_space: SpaceId) -> EventOutcome {
        let Some(entry) = self.display_archive.entries.get(uuid) else {
            return EventOutcome::default();
        };
        let old_space = entry.space;
        let tracked: Vec<(WindowId, Option<WindowServerId>)> = entry
            .windows
            .iter()
            .filter(|window| self.state.windows.window(window.wid).is_some())
            .map(|window| (window.wid, window.wsid))
            .collect();

        // Even when the space survived and nothing left it, the layout is
        // put back from the snapshot: while the tree was squeezed onto the
        // other display, macOS re-laid its windows and the resize reports
        // were folded into the split ratios. The snapshot has the ratios
        // from before.

        // Carry the old space's workspaces over to the new id. This is also
        // what stops the old id's state leaking forever. Not when another
        // display is still showing the old space (the clamshell case above):
        // those workspaces are that display's now, and the snapshot is
        // restored into the new space's own workspaces instead.
        // "In use" means listed as a desktop of any display, shown or not:
        // remapping a desktop's workspaces away from under it leaves that
        // desktop with no tree the next time it is shown.
        let old_space_in_use = self
            .space_state
            .screens
            .iter()
            .any(|screen| screen.space == Some(old_space) && screen.display_uuid != uuid)
            || self
                .space_state
                .display_space_ids
                .iter()
                .any(|(display, spaces)| display != uuid && spaces.contains(&old_space));
        if !old_space_in_use {
            self.layout_manager.layout_engine.remap_space(
                &mut self.state.windows,
                old_space,
                new_space,
            );
        }
        self.layout_manager
            .layout_engine
            .update_space_display(new_space, Some(uuid.to_string()));

        // Every window with a window-server id is waited for, whether or not
        // the addition took the request: the window server is the only
        // authority on where a window is, and a window that arrives on the
        // new space by any route counts. Ones that never arrive are cut off by
        // the deadline.
        let mut waiting = HashSet::default();
        let mut refused = 0usize;
        let addition = scripting_addition::is_available();
        for (wid, wsid) in &tracked {
            if self.assigned_space_for_window_id(*wid) == Some(new_space) {
                continue;
            }
            let Some(wsid) = wsid else {
                continue;
            };
            if !addition
                || !scripting_addition::move_window_to_space(wsid.as_u32(), new_space.get())
            {
                refused += 1;
            }
            waiting.insert(*wid);
        }
        if refused > 0 {
            warn!(
                display = %uuid,
                refused,
                scripting_addition = addition,
                "Windows could not be sent back to the returned display; macOS 26 has no other way to move them (see sys::scripting_addition)"
            );
        }
        info!(
            display = %uuid,
            old_space = old_space.get(),
            new_space = new_space.get(),
            moving = waiting.len(),
            "Display returned; homing its windows"
        );

        let entry = self.display_archive.entries.get_mut(uuid).expect("checked above");
        entry.space = new_space;
        let immediate = waiting.is_empty();
        entry.homing = Some(Homing {
            space: new_space,
            waiting,
            started: Instant::now(),
        });
        if immediate {
            return self.finish_display_homing(uuid);
        }
        self.schedule_homing_deadline(uuid.to_string());
        EventOutcome::default()
    }

    fn schedule_homing_deadline(&self, uuid: String) {
        let Some(sender) = self.communication_manager.events_tx.clone() else {
            return;
        };
        queue::main().after_f_s(
            Time::new_after(Time::NOW, HOMING_DEADLINE.as_nanos() as i64),
            (sender, uuid),
            |(sender, uuid)| sender.send(Event::DisplayHomingDeadline(uuid)),
        );
    }

    /// Runs after every event while a return trip is in flight: once every
    /// window asked to move home is reported on the new space, restore.
    pub(super) fn advance_display_homing(&mut self) -> Option<EventOutcome> {
        if !self.display_archive.any_homing() {
            return None;
        }
        let mut outcome = EventOutcome::default();
        let uuids: Vec<String> = self.display_archive.entries.keys().cloned().collect();
        for uuid in uuids {
            let landed = {
                let entry = self.display_archive.entries.get_mut(&uuid)?;
                let Some(homing) = entry.homing.as_mut() else {
                    continue;
                };
                let target = homing.space;
                let assigned: HashSet<WindowId> = homing
                    .waiting
                    .iter()
                    .copied()
                    .filter(|wid| {
                        self.state.windows.window(*wid).is_none()
                            || self
                                .layout_manager
                                .layout_engine
                                .virtual_workspace_manager()
                                .workspace_info_for_window_any(&self.state.windows, *wid)
                                .is_some_and(|info| info.space == target)
                    })
                    .collect();
                homing.waiting.retain(|wid| !assigned.contains(wid));
                homing.waiting.is_empty()
            };
            if landed {
                outcome.absorb(self.finish_display_homing(&uuid));
            }
        }
        Some(outcome)
    }

    /// The deadline fired: restore with whatever has landed.
    pub(super) fn handle_display_homing_deadline(&mut self, uuid: &str) -> EventOutcome {
        let Some(entry) = self.display_archive.entries.get(uuid) else {
            return EventOutcome::default();
        };
        let Some(homing) = entry.homing.as_ref() else {
            return EventOutcome::default();
        };
        if !homing.waiting.is_empty() {
            warn!(
                display = %uuid,
                still_away = homing.waiting.len(),
                waited_ms = homing.started.elapsed().as_millis(),
                "Not every window made it home before the deadline; restoring without them"
            );
        }
        self.finish_display_homing(uuid)
    }

    fn finish_display_homing(&mut self, uuid: &str) -> EventOutcome {
        let Some(entry) = self.display_archive.entries.remove(uuid) else {
            return EventOutcome::default();
        };
        let Some(homing) = entry.homing else {
            return EventOutcome::default();
        };
        let space = homing.space;

        // Windows that made it home get their own float/tile choice back; the
        // snapshot decides how they are projected. Ones still away keep the
        // float override, or the app rules would tile them into a tree on a
        // display they do not belong to.
        for window in &entry.windows {
            if homing.waiting.contains(&window.wid) {
                continue;
            }
            if self.state.windows.window(window.wid).is_some() {
                self.state.windows.restore_user_floating(window.wid, window.user_floating);
            }
        }

        let request = RestoreRequest {
            scope: RestoreScope::Space,
            active_space: space,
            source: RestoreSource::CurrentSpace,
        };
        let layout_settings = self.config.settings.layout.clone();
        match self.layout_manager.layout_engine.restore_layout_from_snapshot(
            &entry.snapshot,
            request,
            &mut self.state.windows,
            &layout_settings,
        ) {
            Ok(report) => {
                info!(
                    display = %uuid,
                    space = space.get(),
                    matched = report.matched,
                    unmatched = report.unmatched,
                    "Restored the display's layout"
                );
                // The restored state is the user's intent for these windows,
                // so it outranks the app rules from here on: a window the
                // snapshot tiles has no other defence against a catch-all
                // floating rule when it next appears on a space.
                for window in &entry.windows {
                    if homing.waiting.contains(&window.wid)
                        || self.state.windows.window(window.wid).is_none()
                        || self.assigned_space_for_window_id(window.wid) != Some(space)
                    {
                        continue;
                    }
                    let floating = self.layout_manager.layout_engine.is_window_floating(window.wid);
                    self.state.windows.set_user_floating(window.wid, floating);
                }
            }
            Err(error) => {
                warn!(display = %uuid, space = space.get(), %error, "Could not restore the display's layout");
                return EventOutcome::default();
            }
        }

        let size = self
            .space_state
            .screens
            .iter()
            .find(|screen| screen.space == Some(space))
            .map(|screen| screen.frame.size)
            .unwrap_or_else(|| CGSize::new(0.0, 0.0));
        let mut outcome = EventOutcome::window_membership_changed(false, true);
        if size.width > 0.0 && size.height > 0.0 {
            outcome = outcome.with_layout_event(LayoutEvent::SpaceExposed(space, size));
        }
        outcome.with_force_window_refresh().with_arrange_passes(1)
    }
}
