# Performance audit

A read-only audit of where rift spends time it does not need to, taken on
2026-09-02 against `main` at `19078d0` (0.5.3-plus.1) with the working tree's
uncommitted edits. Nothing was changed. Every finding below was verified in
the source; line numbers are from that tree and will drift.

Rendered version with the same content:
<https://claude.ai/code/artifact/aae246c4-e78d-4e7b-91b5-0f84d780b68c>

Frequencies: *per event* means every reactor event, *per pass* means every
layout arrange, *per frame* means every animation tick.

## The headline

Two always-on instrumentation layers tax every event on every thread, and the
arrange path does far more work than one layout: it re-lays out every inactive
workspace for a menu bar that is off by default, builds IPC snapshots nobody
subscribes to, and runs one full pass per window in a batch. The first six
items in the list below need no restructuring and remove most of the fixed
cost.

## Start here

Suggested order. Each is a small, local change with a large share of the win.

1. **Drop the hot `#[instrument]` spans to debug level and make the timing
   layer opt-in.** Every reactor event and every AX request Debug-formats its
   whole payload at INFO and takes a global write lock. (A2)
2. **Gate the menu-bar update on `menu_bar.enabled`, and the broadcast
   snapshots on having subscribers.** Removes N inactive-workspace layouts plus
   two tree snapshots per arrange in the default config. (B1, B6)
3. **Arrange once per event batch instead of once per changed layout event.**
   App launch with 10 windows currently runs 11 full passes. (B2)
4. **Make the live space query in `resolve_native_space` lazy, and resolve each
   window once on a space switch.** Cuts roughly four SkyLight round trips per
   visible window off the space-switch latency path. (B3, B4)
5. **Hoist the float-frame refresh out of the per-screen loop.** One IPC per
   float per display per pass becomes one per float. (B5)
6. **Serialize each flight-recorder line once, lock once, take the String by
   value.** Three JSON encodes and three mutex acquisitions per line today,
   from every thread. (A1)
7. **Replace the per-window AX role sweep on main-window change with a batched
   window-server check.** Twenty windows in an app means twenty blocking IPCs
   on every click between them. (D1)
8. **Prune the layout snapshot through a borrowed view and move the fsyncs off
   the reactor.** Today: RON serialize, parse back, prune, serialize again, two
   fsyncs, on the reactor thread. (C1)

Impact labels (High / Medium / Low) are weighted by how often the path runs in
a default configuration.

## A. Always-on instrumentation

These cost something on every event regardless of what the event does.

### A1. The flight-recorder ring encodes each line three times behind three global locks

**High.** Per event, per system query, per animation frame, from all threads.

`src/sys/trace.rs:76, 153-165, 180-190, 212-260, 403`;
`src/actor/reactor/replay.rs:124-129`.

The recording flag starts true and stays true, so the "one atomic load" fast
path never fires. A reactor system query serializes the key, the answer, and
then the whole `SysLine` again (escaping the answer JSON inside JSON), locks
`MODE` for the timestamp, locks it again in `write_line`, then locks `RING` and
copies the String a second time. Every reactor event is JSON-encoded the same
way, including `MouseMoved` and the large `WindowsDiscovered` and
`SpaceStateChanged` payloads. App threads call `note_write` per animation frame
per window and contend on the same two mutexes, as do the event tap and focus
resolver via `act`.

*Fix:* take the String by value; pass the timestamp in so the lock is taken
once; hand-format the Sys line or use `RawValue` for the answer; keep the clock
start in an atomic instead of behind `MODE`. Consider a per-thread buffer
merged on dump, and downsampling `MouseMoved`.

### A2. INFO-level spans Debug-format their entire payload and take a global write lock

**High.** Per event, per AX request, per AX notification, per pointer sample in
click mode.

`src/actor/reactor.rs:1425`; `src/actor/app.rs:802, 1128`;
`src/actor/stack_line.rs:133`; `src/actor/drop_overlay.rs:101`;
`src/common/log.rs:11-33`.

`#[instrument]` defaults to INFO and the default filter is `rift_wm=info`, so
these spans are live in production. tracing-tree's span data eagerly runs
`format!("{:?}")` on every field at creation, even with deferred spans, so each
reactor event is rendered to a String that is only printed if something logs
inside it. The app-thread spans format `self.app`, an `AXUIElement` whose Debug
impl calls `CFCopyDescription`, on every request including each animation
frame. tracing-timing's `on_new_span` takes a process-wide RwLock write per
span across the reactor, input, and every app thread. Its histograms are read
only by `rift execute show-timing`.

*Fix:* `level = "debug"` on the hot handlers, or skip the payload field and
record a discriminant name. Install the timing layer only behind an env var.

### A3. The reactor clock and mouse-state read go through the trace machinery

**Low.** Per arrange per window; per frame report.

`src/sys/trace.rs:190`; `src/sys/event.rs:96-102`; `src/model/tx_store.rs:33`;
`src/actor/reactor/events/window.rs:245, 283`.

`trace::now()` locks the global `MODE` mutex on every call and is used per
window in `WindowTxStore::insert` and after every event for drop-pin checks.
`get_mouse_state()` wraps a relaxed atomic load in a full `observe` round (A1)
and is called for frame reports that lack a mouse state.

*Fix:* an atomic "replaying" flag so live mode is a plain `Instant::now()`;
cache the last observed mouse state and write a Sys line only when it changes.

## B. Reactor and the arrange path

One layout should be one layout. Today an arrange fans out into several tree
passes and dozens of SkyLight round trips.

### B1. Every arrange lays out every inactive workspace to feed a menu bar that is off by default

**High.** Per arrange.

`src/actor/reactor/query.rs:262-290, 317-431`; `src/actor/reactor.rs:2897,
6727`; `src/actor/menu_bar.rs:152-164`.

`maybe_send_menu_update` runs after every arrange pass. Its only early return
is when the menu channel is absent, but the menu actor always exists. It calls
the workspace query, which for every non-active workspace runs
`calculate_layout_for_workspace` (a full tree layout), a container-tree
snapshot, and per-window data cloning, then builds the window list a second
time. The menu actor then discards it when the icon is disabled or the
signature is unchanged. `rift.default.toml` ships with the menu bar disabled,
and the predicted positions are only used by the layout display style.

*Fix:* gate on `config.settings.ui.menu_bar.enabled` in the reactor; skip
predicted positions unless the display style needs them; or let the menu actor
pull after its 150 ms debounce.

### B2. One full layout pass per changed window in a batch

**High.** Per layout event with `changed = true`.

`src/actor/reactor.rs:2870-2872, 5344-5350`;
`src/layout_engine/engine.rs:1738-1743`.

`apply_event_outcome` loops over `outcome.layout_events` and
`send_layout_event` calls `update_layout_or_warn` whenever the response says
changed, which `WindowObserved` does on any membership change. An app arriving
with N windows drives N complete passes over all active spaces (tiled layout,
float bookkeeping, hidden-window placement, group containers, stack-line and
broadcast events, an `Animation` build) and then the outcome's own arrange
runs one more. Downstream frame diffing stops redundant AX writes but not the
CPU.

*Fix:* accumulate `changed` across the batch and arrange once at the end of
`apply_event_outcome`.

### B3. `resolve_native_space` asks the window server even when the answer is discarded

**High.** Per frame report, per hover change (twice), per focus resolution.

`src/actor/reactor.rs:4765`; `src/sys/window_server.rs:458-475`;
`reactor.rs:3816, 4712-4795, 5436`.

`let live = window_server::window_space(wsid)` runs before the match, but only
the observed-differs-from-pending arm and the no-observation arm read it. The
common `(Some(observed), _)` arm ignores it. Each call is
`SLSCopySpacesForWindows` plus `SLSSpaceGetType` per returned space, each
wrapped in `observe` (A1). It sits under `best_space_for_window` on every user
drag or resize report, under `MouseMoved` twice (once directly, once inside
`should_raise_on_mouse_over`), under window finalization, and under focus
resolution after most events.

*Fix:* compute `live` lazily inside the two arms that need it; pass the
resolved space into `should_raise_on_mouse_over`.

### B4. A space switch resolves every visible window against the window server twice

**High.** Per space change, per visible window.

`src/actor/reactor.rs:903-928, 938-970`; `reactor.rs:847, 898, 3214`.

`authoritative_active_space_windows` calls `resolve_native_space` for every
window on every active space, then `refresh_active_space_window_membership`
takes that already-resolved list and calls it again per entry. Both run on
every `SpaceStateChanged` and every active-space change. With B3 that is about
four synchronous IPCs and four ring lines per visible window on the
user-visible switch latency path.

*Fix:* resolve once and have the second pass trust the passed-in space. With
B3 fixed the common case does no IPC at all.

### B5. Floating frames are re-read from the window server once per screen per arrange

**High.** Per pass × displays × floats.

`src/actor/reactor/managers.rs:377`; `src/actor/reactor.rs:4890-4912`.

`refresh_floating_frames_from_window_server` is called inside the
`for screen in screens` loop. It scans all windows and does one
`live_window_frame` SkyLight query per float, so two displays and six floats is
twelve IPCs per pass, and passes fire per `WindowResized` during a resize
drag.

*Fix:* hoist above the loop; restrict to floats on the spaces being arranged;
skip floats already refreshed by a `WindowFrameChanged` since the last pass.

### B6. Broadcast snapshots are built with zero subscribers

**Medium.** Per layout change, per selection change.

`src/actor/reactor.rs:3328-3368`; `src/actor/reactor/query.rs:585-640`;
`src/actor/reactor/managers.rs:536`; `src/ipc/subscriptions.rs:930-971`.

A full `LayoutStateData` (tree snapshot plus a second workspace layout
calculation plus window filtering) is built on the reactor thread whenever
`broadcast_layout_changed` is set, which is the default. `StacksChanged` is
built per space per pass. The subscriber check happens later in the IPC server,
which drops the event when nobody is listening.

*Fix:* expose a "has subscribers for kind" flag from `ServerState` and skip
building when zero; or make `LayoutChanged` carry ids only.

### B7. Group containers are re-derived and `StacksChanged` is sent every pass, unchanged or not

**Medium.** Per pass per space.

`src/actor/reactor/managers.rs:463-540`;
`src/layout_engine/systems/traditional.rs:1476-1560, 1613`.

`collect_group_containers` runs for every space regardless of whether the
stack line is enabled or anyone subscribes. It re-walks the tree and
`calculate_child_frame_in_container` collects the sibling Vec with an O(k)
prefix loop per child, so O(k²) per container, ignoring constraints so its
rects can disagree with the real pass. The reactor then builds `GroupInfo`
(cloning window id Vecs) and `StackInfo` (a `to_debug_string` per window) and
sends on every pass.

*Fix:* gate on stack-line enabled or subscribers present; emit container rects
as a by-product of `calculate_layout`; diff against the last sent snapshot.

### B8. `sync_float_drag_strips` walks every window on every event

**Low.** Per event.

`src/actor/reactor.rs:4104-4135, 1531`.

Called unconditionally from `dispatch_workflow`. Iterates all windows, does a
floating lookup each, collects a Vec, and compares to the previous one.

*Fix:* a dirty flag set by float toggles, float frame changes, create/destroy,
and config updates.

### B9. `Animation::new` deep-clones the whole `Config` per space per arrange

**Low.** Per pass per space.

`src/actor/reactor/animation.rs:152, 545`; `src/common/config.rs:372`.

The animation reads two `f64` settings but receives a clone of `Config`, which
carries key specs, app rules, and several `Vec<String>` lists.

*Fix:* pass the interval and frame count, or an `Arc<Config>`.

### B10. `admitted_root_for` queries a live frame per admitted sibling

**Low.** Per focus change onto a non-admitted window.

`src/actor/reactor.rs:4846-4890`.

For a focused panel or sheet, it calls `live_frame_for` for the child and for
every admitted window of the same pid, on every `WindowServerFocusChanged` and
`ApplicationMainWindowChanged`.

*Fix:* use `frame_monotonic` for candidates and query live only for the child;
memoize child→root until the child's frame changes.

### B11. Per-pass and per-snapshot churn

**Low.** Per pass; per space snapshot.

`src/actor/reactor/managers.rs:340-377, 505`; `src/actor/reactor.rs:3411,
3563`; `src/model/window_store.rs:289`; `src/model/virtual_workspace.rs:969`.

`space_state.screens.clone()` per pass; `tracked_window_count()` is an O(N)
scan used as an emptiness check; `active_workspace_idx` clones, retains, and
sorts the workspace id Vec every call; `handle_authoritative_space_snapshot`
clones the whole `ForwardedSpaceState` up front but only uses the clone in the
Mission Control branch.

*Fix:* keep a tracked-window counter; keep workspaces sorted at mutation time;
clone the snapshot inside the branch.

## C. Layout engine and persistence

The tree itself is sound. The costs are around it: membership bookkeeping,
constraint recomputation, and the save path.

### C1. The layout snapshot round-trips the engine through RON three times, then fsyncs twice on the reactor thread

**High.** Per 60 s autosave; per `WindowRemovedPreserveFloating` (10 s TTL);
per native-fullscreen entry.

`src/layout_engine/engine/persistence/storage.rs:226-233, 282-286, 307-330`;
`src/actor/reactor/display_archive.rs:225`;
`src/actor/reactor/fullscreen_slots.rs:72`.

`loadable_snapshot` serializes every tree, workspace, and fingerprint to a RON
String, parses it back into an owned `PersistedLayout`, prunes, and then the
save serializes it again. The write then does `file.sync_all()` and a directory
`sync_all()` on the reactor thread. `prepare_persisted_state` additionally runs
a tree scrub per float and a full retain per tiled window. The light snapshot
fires on window removal and on fullscreen entry, exactly when the reactor is
already busy.

*Fix:* prune via a borrowed view so one `ron::to_string` suffices; hand the
string to a blocking thread for the write and fsyncs. The shutdown save can
stay synchronous.

### C2. `WindowObserved` does O(workspaces × layouts log layouts) bookkeeping per window

**Medium.** Per observed window: app launch, space activation, rule reapply.

`src/layout_engine/engine.rs:1235-1240, 1250-1265, 1642, 1663-1673, 1701`;
`src/layout_engine/workspaces.rs:249-260`.

For every workspace on every space it calls `workspace_contains_window`, which
builds, sorts, and dedups a Vec of every (space, workspace, layout) triple via
`all_layouts()`, then removes the window from every tree anyway.
`space_with_window` runs twice per event, each allocating the active float
list. The trees already answer membership in O(log n).

*Fix:* have `remove_window` return whether it removed anything and drop the
contains check; compute `all_layouts()` at most once per event.

### C3. Tree layout recomputes subtree constraints at every level

**Medium.** Per pass, O(n × depth).

`src/layout_engine/systems/bsp.rs:513, 716, 1725`;
`src/layout_engine/systems/traditional.rs:2714, 2919, 3074`.

BSP's recursive layout calls `subtree_axis_constraints` for both children of
every split; that helper walks the entire subtree and allocates four Vecs per
split. A BSP grown by always splitting the focused leaf has depth near n, so
the pass is quadratic in walks. Traditional (which also backs stack and
master-stack) has the same pattern in `node_axis_constraints` with five Vecs
per container. BSP also fills a `nodes` Vec at 1725 that is discarded.

*Fix:* one post-order pass memoizing (min, fixed, max, can_grow) per node into
a `SecondaryMap`, then the top-down rect pass.

### C4. Startup restore matching is O(pending × layouts) per observed window

**Medium.** Per observed window while `pending_windows` is non-empty.

`src/layout_engine/engine/persistence/reconcile.rs:141-172, 190-205, 226`.

Each live window builds a candidate for every pending saved window, each
calling `restored_locations_for_window`, which sorts `all_layouts()`, checks
every layout, and linearly scans plus sorts float positions per (space,
workspace). Then it repeats for the winner. Fifty saved windows across forty
layouts is millions of operations during the discovery storm, when the reactor
is busiest.

*Fix:* maintain a pending→locations map, rebuilt only when the pending set or
the trees change.

### C5. Per-screen-size layout clones are never pruned

**Low.** Per new pixel size; compounds C1, C2, C4.

`src/layout_engine/workspaces.rs:17, 128-170`.

`ensure_active_for_space` clones the full tree for every new screen size and
keeps all of them. Every dock toggle, resolution change, or hot-plug leaves a
permanent clone per workspace, all serialized on every save and enumerated by
every `all_layouts()` consumer and scrub.

*Fix:* keep at most two or three configurations per workspace (LRU), or drop
non-active ones on save.

### C6. Allocation churn inside the virtual-workspace layout pass

**Low.** Per pass, per window.

`src/layout_engine/engine.rs:557, 617, 1561, 2421-2720, 2796`;
`src/model/floating_position_store.rs:130-143`;
`src/model/virtual_workspace.rs:784, 837-870`;
`src/model/window_store.rs:735-748`.

`workspace_positions` scans all stored floats across all spaces and sorts, per
pass. `active_floating_windows_in_workspace` runs twice per pass. The
hidden-position helpers allocate an `others` Vec per window per call.
`windows_in_inactive_workspaces` allocates and sorts per inactive workspace.
`list_workspaces(space).to_vec()` clones an already-owned Vec.

*Fix:* compute `others` once per pass; reuse one floats Vec; return slices or
iterators.

## D. The macOS edge

Accessibility and SkyLight round trips are the expensive primitive here. Each
AX call blocks on the target app's main thread, bounded only by the 1 s global
timeout.

### D1. One AX round trip per tracked window on every main-window change

**High.** Per `AXMainWindowChanged`, per window of that app.

`src/actor/app.rs:1146, 1176, 1970-1984`.

`remove_stale_windows` reads `elem.role()` for every window the app has, on
every main-window change, before the actual main-window read. Every click
between an app's windows pays this. An app with 20 windows means 20
synchronous IPCs into its main thread. The same sweep runs on
`WindowDestroyed` when the id fails to resolve.

*Fix:* resolve staleness from the window server with a batched `get_windows`
on the known wsids, or sweep on a cadence.

### D2. One global raise mutex held across AX calls and a 10 ms activation poll

**Medium.** Per raise.

`src/actor/app.rs:1383-1471, 1661-1666`; `src/actor/raise_manager.rs:63`.

`handle_raise_request` takes a process-wide mutex and under it calls
`frontmost()`, `make_key_window`, `raise()`, and `wait_for_activation`, which
polls `frontmost()` every 10 ms. One unresponsive app blocks raises for every
other app; the raise manager's 250 ms timeout cancels the token but cannot
release the lock.

*Fix:* scope the mutex to the make-key plus raise adjacency; rely on the
`AXApplicationActivated` observer already wired instead of polling.

### D3. Per-window space lookup on every space snapshot, used only in the conflict branch

**Medium.** Per space change, screen refresh, display stabilization; per
visible window.

`src/actor/spaces.rs:963-976, 997-1029`; `src/sys/window_server.rs:458`.

`build_forwarded_state` calls `window_space` for every window on every active
space. `record_visible_window_space` reads that value only in the `Occupied`
conflict arm. Forty visible windows means roughly 80 extra SLS calls per
snapshot.

*Fix:* compute `window_space` lazily inside the `Occupied` arm.

### D4. Window registration is about 14 IPCs per window, with a duplicate and a full-app refresh trigger

**Medium.** Per new window; per `WindowMaybeDestroyed` for the whole app.

`src/sys/app.rs:418-487`; `src/actor/app.rs:470-540, 1774, 1828-1848`;
`src/actor/reactor/events/space.rs:159, 226`.

`WindowInfo::from_ax_element` does six AX reads plus `window_parent` and
`bundle_info_for_pid` per window. `register_window` then calls `window_parent`
again and registers six notifications. `refresh_window_inventory` re-runs the
full read for every window of the app, and the reactor requests it whenever
one tracked window leaves a space.

*Fix:* drop the second `window_parent`; cache bundle info per pid; in refresh,
read only `minimized` for unchanged elements.

### D5. Frame writes are three AX sets and one read, unconditionally

**Medium.** Per non-animated frame write.

`src/actor/app.rs:670-672, 940-945, 980-985, 1081-1083`.

Set size, set position, set size, then read frame. The leading set-size works
around apps that clamp size on move, but it runs even for pure moves where the
size is unchanged. Batches are sequential on the app thread, so an arrange of
8 windows in one app is 32 blocking IPCs. The animation path already avoids
this by setting size on only two of N frames.

*Fix:* skip the leading set-size when the desired size equals the last known
size; skip the trailing one when the readback already matches.

### D6. Focus-follows-mouse raise check lists the whole space and does a Mach IPC per candidate

**Low.** Per hover change.

`src/actor/reactor.rs:5473-5508`; `src/sys/mach.rs:622-699`.

`should_raise_on_mouse_over` fetches the full z-order of the space, then
`window_level` and `window_sub_level` (a synchronous Mach round trip with a
special reply port) for the candidate and every floating window above it. The
event tap already dedups so this is per transition, not per move.

*Fix:* only query levels when a floating window's frame contains the
candidate; cache levels per wsid until a `WindowLevelChanged` event.

### D7. `live_window_frame` always fetches size constraints too

**Low.** Per float frame change, per drag update, per `AXWindowMoved` with
button down.

`src/sys/window_server.rs:211-230, 640-654, 750-766`.

It goes through `get_window`, whose `window_info_from_query` calls
`constraints()`, which is `SLSWindowIteratorGetConstraints` and, when zero, a
second `SLSPackagesGetWindowConstraints` round trip. Only the bounds are
needed.

*Fix:* a bounds-only variant reading from the iterator directly.

### D8. Smaller items on the system edge

**Low.**

`src/sys/scripting_addition.rs:173-180, 257-289`;
`src/sys/window_notify.rs:269-273`; `src/actor/window_notify.rs:207`;
`src/actor/spaces.rs:644, 862, 902`; `src/sys/screen.rs:344-391, 547-564`.

The scripting-addition client opens a socket per command and blocks the
reactor up to 400 ms on a slow Dock; `is_available()` connects on every call.
CGS event payloads are copied with `to_vec()` into a field nothing reads, and
one OS thread is spawned per subscribed event type. `build_forwarded_state`
looks up the active menu-bar space twice. The dirty screen refresh repeats dock
and menu-bar queries per display and re-fetches `NSScreen::screens` per
display for the notch.

*Fix:* cache availability with a short TTL; drop the payload field; one thread
selecting over receivers; hoist the dock queries out of the per-display loop.

## E. UI and IPC

Mostly bounded to when a feature is on. The stack line is the one that runs
by default when enabled.

### E1. Stack-line indicators recreate CALayers and rasterize on every update

**Medium.** Per `GroupsUpdated`, per indicator.

`src/ui/stack_line.rs:186, 253-258, 388, 427-462, 518, 558-602, 692-695`;
`src/actor/stack_line.rs:231-237, 429-470`; `src/ui/common.rs:12-31`.

The background and selected layers are removed and recreated on every update
because the corner radius is hardcoded, which leaves the layer-reuse paths
dead. `present()` then renders the tree with `renderInContext` into an SLS
window context and reorders. The actor calls `set_visibility` (an SLS order
call) on every indicator on every update even when the group signature says
nothing changed, and clones group data three to four times per update.

*Fix:* keep the layers and set frame and colors; remember visibility and skip
no-op ordering calls.

### E2. CLI subscription exec copies the environment and spawns synchronously on the broadcast bridge thread

**Medium.** Per event per CLI subscription.

`src/ipc/cli_exec.rs:489-742`; `src/bin/rift.rs:316-329`.

Per event it clones the env map, copies the full process environment into
CStrings, serializes the event JSON, and does a blocking `posix_spawnp` on the
thread that also feeds the Mach dispatch queue, so a slow spawn delays Mach
subscribers. A new executor is built per call.

*Fix:* cache the base envp; spawn from a dedicated thread or dispatch queue.

### E3. Mission Control pre-fetches workspaces synchronously on every space update while hidden

**Medium.** Per `SpaceStateUpdated` and workspace switch, when enabled.

`src/actor/mission_control.rs:189-191`; `src/actor/wm_controller.rs:192-201`;
`src/ui/mission_control.rs:1692-1728`.

`refresh_snapshot` does a blocking cross-thread `query_workspaces` round trip
from the main thread on every `RefreshCurrentWorkspace`, whether or not the
overlay is showing, and that query includes the inactive-workspace layouts
from B1. While shown, the exploded layout is recomputed per mouse move.

*Fix:* query lazily in the show path, not on every space update.

### E4. Client, config, and build profile

**Low.**

`crates/rift-client/src/lib.rs:440-497`; `src/actor/config.rs:186-234,
263-271`; `src/actor.rs:39-41`; `Cargo.toml:26-33, 184-185`.

The Mach client does a bootstrap lookup and port allocate/deallocate per
request, and sleeps up to 1.5 s when rift is absent; fine for the one-shot CLI,
wasteful for long-lived library users. The config actor validates twice and
round-trips through `serde_json::Value` on Set, both rare. Every actor send
captures the current Span, which is cheap once A2 is fixed. The release
profile is already near-optimal: thin LTO with one codegen unit,
`panic = "unwind"` is required by `catch_unwind` sites, and fat LTO would buy
perhaps a few percent for a much longer link. tokio pulls default features
through tokio-stream and tokio-util, and tracing-timing plus hdrhistogram exist
only for show-timing, both build-size only.

*Fix:* cache the service port in the client; leave the profile alone.

## Already efficient

Verified so nobody re-optimizes what is already right.

- **Event tap.** Scalar timestamp gate before `catch_unwind`; `MouseMoved`
  only in the mask when needed; window resolution reuses the CGEvent hint;
  transitions deduped before the reactor; hotkeys and hit rects via `ArcSwap`.
- **Reactor loop.** Drains up to 64 events per wake; queries bypass
  quarantine; inventory requests deduped per pid.
- **Drag path.** Candidates from tree membership, no server queries; overlay
  aims deduped; drop-pin probes throttled; inventory sweeps deferred during
  drags.
- **Animation.** Timer polled only while active; per-frame work is arithmetic
  plus sends; targets equal to the current or pending frame are skipped; app
  threads coalesce frames so slow AX drops frames instead of queueing.
- **Tree and indexes.** Slotmap-backed tree with lazy iterators; FxHash
  everywhere; `WindowStore` indexed in every queried direction; BSP and
  traditional index windows to nodes in O(log n).
- **Debug checks.** Invariant assertions gated on `debug_assertions`; tree
  renders sit inside `debug!` so they are free at the default filter.
- **Window server.** `get_windows` batches ids into one query; key-focused
  window is a server-side filtered query; connection and query keys resolved
  once.
- **Focus resolver.** 1 ms debounce coalescing CGS bursts into one resolution.
- **App actor.** One thread per app with request batching and enhanced-UI
  suppression per batch; server frame instead of AX while the button is down.
- **IPC fan-out.** Serialize once per event, bounded worker queue, one shared
  CString for all targets, non-blocking Mach send.
- **Config.** App rules precompiled with regexes; FSEvents watcher with 250 ms
  debounce; no polling.
- **Menu bar and drop overlay.** 150 ms debounce and signature short-circuit;
  glass view moved with `setFrame`; 60 Hz timer only while moving.
- **Persistence.** Borrowed serialization view, atomic temp plus rename, 60 s
  timer autosave.
- **Dock payload.** The only per-tick hook is a handful of float ops with a
  cached timebase.
