# Handoff: cross-display drags, floats, and the trace replay harness

Branch: `feat/display-layout-restore`. Everything below is committed in `3646b88` on top of
`15a4651` (one commit; the suggested split was not done). Rounds 2–6 are the 2026-08-30 afternoon
passes, each driven by a recording.
Experiments that were abandoned live on `wip/drag-freeze-experiments` (reference only).

## What was wrong, in one sentence

Rift keeps several independent records of the same fact (which tree a window is in,
which workspace it is assigned to, whether it floats, where it was last seen, which window
is focused, which display it is on) and reconciles them with heuristics; every bug in this
round was two of those records disagreeing, and rift acting on the stale one.

## Fixes on this branch (each has a test; most have a recorded fixture)

| Symptom | Root cause | Fix | Where |
|---|---|---|---|
| sa+T from the Dock tiled the *previous* window | Premiere has no `AXMainWindow`; engine's private `focused_window` stayed on the last managed window | `AXFocusedWindow` fallback; reactor aligns engine focus to the resolved front window before focused-window commands; unmanaged front window ⇒ command is a no-op | `sys/axuielement.rs`, `actor/app.rs::read_main_window`, `reactor.rs::sync_layout_focus_for_command` / `focused_window_is_unmanaged` |
| Preview Open panel snapped/resized | window server reports a new panel as 0×0; float layout centred it | empty frames never adopted; float layout never invents a frame (no centring fallback) | `reactor.rs::refresh_floating_frames_from_window_server`, `engine.rs` float layout |
| Cross-display drop of a tile → flicker between displays | drop reassigned the window *before* the deferred `WindowRemoved`, which removed from the wrong tree ⇒ tiled on both spaces | removal scrubs the window from every tree | `engine.rs::remove_window_layout_membership` |
| sa+T on a float left it tiled + floating (floats "snap back") | toggle removed from the *pointer's* display's tree, not the window's | toggle acts on the window's own space/tree, removes from all trees | `engine.rs` `ToggleWindowFloating` |
| User's tile choice lost on cross-space move | `clear_rule_metadata` wiped `user_floating` | only rule metadata is cleared | `model/window_store.rs` |
| Drag not seen at all ⇒ no drop resolution, stale frame stored at release | transaction gate discarded button-down frame reports trailing rift's last write | button-down reports bypass the gate; window is *held* from the first such report until `MouseUp` | `events/window.rs::classify_window_frame_change`, `reactor.rs` `WindowFrameChanged`/`MouseUp`, `managers.rs::DragManager::held_window` |
| Float jumped to a remembered frame at drop | `pending_float_placement` was per window; a stale stored frame on another workspace became "intended" | placement bound to (space, workspace); drop stores the live frame | `engine.rs::PendingPlacement`, `events/drag.rs` |
| Mid-drag, window pulled into a second tree | discovery `sync_tiled_windows_for_app` re-adds from the store's assignment | engine knows the frozen window; one-tree law enforced on every discovery add; reassign/topology/rule passes skip the held window | `engine.rs::sync_tiled_windows_for_app`, `reactor.rs::window_in_drag` guards |
| `sa+N` / `Alt+N` only addressed the active display's spaces | `space_at_index` was per display | global Mission Control numbering; cross-display switch via scripting addition, pointer-warp + swipe fallback | `sys/space_switch.rs`, `sys/screen.rs` |

Config: `~/dotfiles/rift/.config/rift/config.toml` (and the live copy) gained
`{ app_id = "com.adobe.PremierePro.26", ax_role = "AXLayoutArea", manage = true, floating = true }`
— Premiere's document window is `AXLayoutArea`, so the standard-window heuristic never admits it.

## Round 2: the flicker persisted, and the harness had said "clean"

The fixtures replayed with 0 violations while the cross-display flicker still happened on build
75040. Reading the `user_drag3` replay report by hand (state trajectory + live writes) showed two
mechanisms the invariants could not see:

1. **Mid-drag re-tile before the app reports the drag.** The window server hands a window to the
   display under the pointer as soon as the user grabs it; `SpaceStateChanged` arrives (with the
   window listed on the other space) ~30 ms *before* the app's first frame report with the button
   down, so `held_window` was not yet set and `reconcile_authoritative_active_window_snapshot` →
   `reassign_window_to_authoritative_space…` re-tiled the window under the cursor. The same
   snapshot's `WindowsDiscovered` then re-added it from the primary (window-server-id) loop of
   `emit_layout_events`, which skipped the frozen window only in the AX-fallback loop.
2. **Post-drop chase.** After a cross-display drop rift writes the new display's frame; Premiere
   takes 100–300 ms to apply it; meanwhile the server still reports the old space;
   `resolve_native_space` asked the server again (`live`), got the same stale answer, and believed
   it over the pending write — re-tiling on the old display, whose write landed just as the first
   one did, and so on: five alternating writes in 700 ms.

Fixes (uncommitted, on top of the table above):

| Where | Change |
|---|---|
| `reactor.rs::hold_if_dragged_across_spaces` (called from the reassign funnel) | a window that changes space while the button is down (`get_mouse_state()`) and is tiled on an active space is *held* (`held_window`) instead of reassigned; the drop resolves it |
| `reactor.rs::settle_held_window` (MouseUp) | a held window whose drag the app never reported is reassigned to where the server has it once the button is up |
| `events/window_discovery.rs::emit_layout_events` | the frozen window is skipped in the primary loop too |
| `model/tx_store.rs` + `resolve_native_space` | `TxRecord.sent_at`; a pending frame write younger than `DropPin::HOLD` (1.5 s) is believed over a contradicting space report — except when the report is rift's own homing move landing (`display_archive.homing_destination`) or rift moved the window with the scripting addition (`note_window_sent_to_space` clears the target) |

Harness changes (`replay.rs`) — verified by reverting the fixes: the old code now fails with 23
violations across the drag fixtures, the fixed code with 0 before divergence:

- Invariant 1 is judged at button release: writes since the first `MouseDragged` (or first
  button-down report) are collected, and at `MouseUp` any write to a window the user turned out to
  be moving (reported Down, or the reactor's session) is a violation.
- Invariant 4 no longer consults where the server has the window (the server's report lags rift's
  writes — following it *is* the bounce): two writes of one window to different displays within
  1.5 s with no button release and no command in between is a violation.
- `mouse_state` answers are synthesised from the event stream (`MouseDragged` ⇒ down, `MouseUp`/
  `MouseMoved` ⇒ up), because the recording only has answers for the moments live asked.
- **Divergence detection**: the replay is open-loop. `LiveWrites` matches every replay write to
  the nearest live write (±500 ms, same frame, or a no-op to where the window already was) and
  every `requested` frame report to the replay's last write; the first mismatch sets
  `report.diverged` and later violations go to `after_divergence` (printed, not failing). Every
  drag fixture now diverges within seconds of its first drop — exactly where the fix changed
  rift's writes — so **the fixtures prove the first drop of each recording and nothing after**.

A unit test covers the hold: `window_that_changes_space_with_the_button_down_is_held_until_the_drop`
(`sys::event::set_mouse_state_override` is the thread-local test hook).

**Validation that is still owed:** a live re-record on the new build. Drag a tile between displays a
few times (both directions, drop with the window's centre on either side of the seam), a float
across, and `sa+T` a couple of times; `rift-cli execute trace start ~/Downloads/drag5.trace` …
`trace stop`; copy into `tests/traces/`; `cargo test --lib recorded_traces_replay_cleanly -- --nocapture`.
A recording made on the fixed build should not diverge at all, and its verdict then covers the
whole session.

## Round 3: `user_drag5` (recorded on the round-2 build)

The recording replays with 0 violations before divergence and confirms the flicker fix live. Three
more things came out of it, all fixed:

| Symptom | Root cause | Fix |
|---|---|---|
| Tile dragged 20 % onto the LG snapped back to the laptop | drop space came from the window's centre (`settled_space`) | drop space is the display under the pointer at `MouseUp` (`pointer_space`, before the frame-based fallbacks); a swap target still wins |
| Preview document window refused `sa+T` (admitted=false for the whole session) | the window-server snapshot cached at `WindowCreated` said `layer: 1` (the instant Preview opens a document behind its Open sheet); `refresh_heuristic` compared against the cache forever, while sticky/level were asked live | a would-be rejection re-queries `window_server::get_window` and refreshes the cache (`utils.rs::refresh_heuristic`); a cached layer 0 is not re-asked, so the normal path costs nothing extra. Not Preview-specific: any window created on a transient layer heals on its next refresh |
| Premiere squeezed to a 0-px slot (invariant 2, after divergence) | Mail reported its move halfway applied — new origin, old 2491×1399 size from the LG tile — and `HandledRefusedSize` took that as a minimum wider than the laptop display | a "refused" size larger than the window's display is not recorded as a minimum |

## Round 4: alt-drag resize left overlaps and gaps

A modifier (alt-drag) resize of a tile goes through `WindowResized` → arrange → rift writes the
window and its neighbours; the app echoes each write with the button down. The frame-report path
took "button down" as "the user is moving this window": `held_window` was set, and a held window is
skipped by every arrange — so the neighbours kept following the pointer while the resized window
stayed where the first write put it. `modifier_drag` was also never cleared (only replaced by the
next one), which kept `follow_focus_with_mouse` disabled after the first alt-drag.

Fix: reports for the window under a modifier drag are read as echoes (`effective_mouse_state = Up`
before the hold and the transaction gate), and `MouseUp` clears `modifier_drag`. Test:
`modifier_drag_echoes_do_not_hold_the_window`.

## Round 5: after an alt-drag, the next plain drag was not snapped back until a later click

`user_altdrag.trace`: after an alt-drag (even a no-op one on a lone tile) the user's next plain drag
ends with the window reported away from its slot and **no write**; the snap only comes with the
following drag's release. Cause: the event tap swallows the release that closes a modifier drag (so
the app never sees half a click) and never told the reactor, whose `modifier_drag` stayed armed —
so the next drag's reports were read as echoes of rift's own writes (round 4) and neither held nor
sessioned the window. Fix: the tap sends `Event::MouseUp` for a swallowed release too.
`MouseModifierDragBegin` is now serialisable, so alt-drags replay; this fixture predates that and
its alt-drags are events without a begin.

## Round 6: `user_altdrag2` (alt-drags now recorded), Preview after the file picker, pointer warps

| Symptom | Root cause | Fix |
|---|---|---|
| Alt-resize still left a gap until release | round 4 exempted only the *dragged* window's reports; the neighbour's echo (button down) still set `held_window` and the arrange skipped it | any frame report during a modifier drag is an echo (`modifier_drag.is_some()`) |
| Preview document opened from the file picker refuses `sa+T` | rejected at creation (`layer 1`); the round-3 re-query only runs inside `refresh_heuristic`, and nothing ever called it again for that window — `main_window()` was even `None` for it, so the command path bailed | `readmit_rejected_window`: a rejected window is judged again when it comes to the front and when a focused-window command targets it (all rejected windows of the frontmost app when rift can't name a front window); admitted, it enters the layout as a created window and the command is consumed |
| Clicking a window warped the pointer into it; clicking another display's menu bar warped to that display's app; dismissing a status-item popover warped back to Premiere | `follow_focus_with_mouse` ran on every focus change regardless of cause | `focus_change_is_pointer_driven`: no warp while the button is down or within 500 ms of `MouseUp`; and no warp when focus returns to the window it left for something rift has no window for (`focus_left_from`) |

Tests: `focus_change_right_after_a_click_does_not_warp`, `focus_returning_from_a_windowless_app_does_not_warp`.
The test helper's "unmanageable" windows are now non-standard, so the heuristic agrees with the flag
when a window is re-judged.

## The trace harness (the important deliverable)

Rift can record everything the reactor sees and replay it offline, bit for bit.

Record on the running instance:

    rift-cli execute trace start ~/Downloads/name.trace
    ... reproduce ...
    rift-cli execute trace stop

Drop the file into `tests/traces/`; `cargo test --lib recorded_traces_replay_cleanly` replays every
`*.trace` there (`-- --nocapture` prints the report: requests, writes, state trajectory, drops,
live-vs-replay writes).

Format (`src/actor/reactor/replay.rs`, `src/sys/trace.rs`), one line each:
1. config (JSON — RON cannot round-trip flattened/untagged serde),
2. layout engine snapshot (RON),
3. `Windows {json}` — the window store,
4. `Transactions {json}` — per-window transaction ids/targets (without this, replay discards
   frame reports live accepted — the harness lied until this was added),
then, interleaved: `Ev <ms> <event json>`, `Sys {ms,kind,key,answer}` (every out-of-band system
answer the reactor got, recorded by `trace::observe` wrappers in `window_server`, `screen`,
`space_switch`, `event::get_mouse_state`, `scripting_addition`), `Out {..}` (every frame rift
wrote, hooked in `actor/app.rs`).

Recording starts mid-session, so the header is followed by synthetic `SpaceStateChanged`,
`ApplicationLaunched` (per known app, with its windows), `ApplicationGloballyActivated` and
`WindowServerFocusChanged` events, plus pre-recorded answers for the questions those launches ask.

Replay (`replay_trace`): thread-scoped replay mode; `trace::now()` returns recorded time; answers
are matched by (kind, key) as *state at the current replay time* (all lines ≤ now consumed, latest
wins, reused if asked again); misses and key drift are reported. Invariants checked after every
event / on every write:
1. no frame written to a window between its first button-down report and `MouseUp`;
2. no empty frame written;
3. a window is in at most one tree, and never floating and tiled at once;
4. no unprompted display bounce (writes to a display the window server does not have the
   window on, alternating within 1.5 s);
5. a float is written a frame only on a command's behalf.

Fixtures in `tests/traces/` (`user_drag5` is 13.8 MB; — decide whether to keep them in the repo):
`seam_drops_tiled_premiere` (synthetic, 53 violations without the removal fix),
`seam_drops_after_fix`, `user_drag` (float jump, 535 → 0), `user_drag3` (mid-drag writes),
`user_drag4` (mid-drag second tree; replay writes match live's 14), `user_float`.

Known limits: replay is open-loop — after a fix changes rift's writes, the recorded window-server
reactions no longer correspond; a clean replay of an old trace proves rift's handling of *those*
inputs, a live re-record proves the loop. Sequence-dependent gaps left: `is_point_occluded…`
(event-tap thread) and app-thread round trips (`GetVisibleWindows` gets no reply on replay; the
recorded `WindowsDiscovered` events stand in).

Latent bugs the harness surfaced in rift's own persistence (fixed): `MouseModifier` and
`SpaceStateChanged` weren't serializable; infinite `max_frame` broke JSON; a live engine snapshot
failed its own validation (workspaces on unexposed spaces — healed on snapshot load);
`ensure_space_initialized` underflowed with zero workspaces.

## Build / install (the running rift is this branch, not Homebrew's)

    cargo build --release
    codesign -f -s "Developer ID Application: Eric Wang (8UR4G77744)" -i git.acsandmann.rift --timestamp target/release/rift
    install -m 555 target/release/rift /opt/homebrew/opt/rift/bin/rift
    install -m 555 target/release/rift-cli /opt/homebrew/Cellar/rift/0.5.3/bin/rift-cli
    brew services restart acsandmann/tap/rift

Accessibility is keyed to the signature; a rebuild occasionally needs the toggle in
System Settings → Privacy & Security → Accessibility (rift polls and starts once granted).
Logs: `/tmp/rift_eric.err.log`. `launchctl setenv RUST_LOG rift_wm=debug` before a restart for
debug logs (unset afterwards).

## Open items

- Live validation of round 6 (build of 14:56+): alt-resize with two tiles, Preview opened through
  its picker then `sa+T`, clicks into Warp / another display's menu bar / a status-item popover.
  Anything off: record (`rift-cli execute trace start … / trace stop`), drop into `tests/traces/`.
- `tests/traces/` is 23 MB and in git now. Fixtures recorded before a fix diverge at the point the
  fix changed rift's writes; their verdict covers only the prefix. Re-recording on a current build
  replaces them with recordings that do not diverge (`user_drag5`, `user_altdrag2` are the current
  ones).
- Logging defaults to `rift_wm=info` (`src/common/log.rs`); `RUST_LOG` overrides. Service logs:
  `/tmp/rift_eric.err.log`.
- `window_notify`/spaces-actor and event-tap threads make their own system queries; they reach the
  reactor as events, which is enough for replay but means those actors are not themselves replayed.
- Every `MouseDragged` still triggers `window_spaces` queries for every window (visible in any
  trace: ~20 `Sys` lines per pointer move). Not a correctness problem; worth a look for latency.
