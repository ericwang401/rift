Hey everyone, I've been maintaining my [own fork](https://github.com/ericwang401/rift/) of Rift.

I've been a long time user of [Yabai](https://github.com/koekeishiya/yabai), but over time, I found it really hard to cope with its buggy-ness and the lack of maintainability of the codebase (huge C codebase with no unit tests). Regarding the latter, I actually tried vibe coding a [Rust port](https://github.com/performave/yabai-plus/tree/rust-port) that also ended up being pretty buggy such that I didn't feel like it was worthwhile to deal with it.

I started using Rift recently, wanted to bring some features over from Yabai:

- **Real macOS space control on macOS 26** — switch to a space by number instantly (no animation), move windows between spaces, create/destroy spaces. Every SkyLight route Yabai used is gone on 26, so this talks to the scripting addition's socket inside Dock (with graceful degradation when it's absent — the default space switch stays the unprivileged synthetic gesture).
- **Drag-to-split with a live drop overlay** — dropping on a window's edge inserts and splits instead of always swapping (Yabai's `mouse_determine_drop_action` zones), and an optional Liquid Glass overlay previews exactly where the window will land. Works across displays.
- **Modifier-drag to move/resize** — Yabai's `mouse_modifier` / `mouse_action1` / `mouse_action2`. Floats move directly; tiled windows resize through their split ratios, moving the edges the press landed on.
- **`rotate` / `mirror` / `balance`** — `space --rotate`, `--mirror`, and `--balance` equivalents for the tree layouts (balance weights splits by leaf count, like Yabai's equalize).
- **`mouse_follows_focus` on every focus change** — cmd-tab, clicks into other apps, apps raising their own windows — like Yabai's `WINDOW_FOCUSED` handler, plus a per-app blacklist for screenshot/overlay tools.
- **Directional focus/moves that continue onto the neighbouring display**, and an opt-in `directional_focus_skips_floating` matching Yabai's managed-windows-only behavior.
- **`destroy_workspace`** (`space --destroy`), with windows rehomed to a neighbour first, and `create_space` that inserts next to the active space instead of appending.
- **Display-layout restore** — a display's layout survives unplug/sleep/lid-close and gets put back (with its windows) when the display returns, including native-fullscreen windows going back to their old slot.

Also, I made quite a few bug fixes just from using Rift over the past two days:

- 85 key codes (including the letters I, J, K, L, M, N, O, P, U) rendered as "Other" in the hotkey Display→FromStr round trip and were silently never registered — a stock-shaped config had focus/move on j/k/l dead while h worked.
- The dock orientation constants were wrong (left/right misclassified), so a left-positioned dock made tiled windows start underneath it.
- Catch-all `floating = true` app rules re-floated hand-tiled windows on every space activation, and the re-add path stranded orphan leaves (I observed a live tree holding five leaves for two real windows).
- Closing a window of an app that keeps running (ordered out, never destroyed) left its slot held open until something else touched the space — it's now untiled immediately, verified via its AX element.
- A resize of a window that already filled the screen was swallowed as "self-fullscreen" (#415 regression); `focus_display` didn't move real keyboard focus (#332); `rebalance` existed on the trait but the bsp impl was empty and nothing invoked it.
- A refused drag-insert could strand the dragged window out of every tree; a drop with no swap skipped the arrange it owed; the drop overlay could linger after certain drag endings.
- Floats snapped back after every drag because stored frames were re-asserted forever — a visible float now follows its live frame, and stored frames only apply to intents (parking, restores, explicit placements).
- Same-app focus across displays resolved one click behind; various pointer-warp annoyances (warping onto a window the cursor was already in, warping on pointer-driven focus changes).

I'm interested in eventually merging some if not most of my changes upstream so everyone can all benefit from it. I'm a little hesitant for now given how fragile window managers can be (I swear I would have 10 things break for every one change).

Here's an exhaustive list of changes below, but at this point, I recommend those who are interested to just test the codebase with their workflow to see what comes up:

<details>
<summary>Full changelog (Keep a Changelog format)</summary>

### ⚠ Breaking / intent-diverging changes

These do more than fix bugs — they change behavior upstream users may rely on, or steer things in a direction upstream may not have intended (broadly: toward Yabai parity, real macOS spaces, and heavier instrumentation).

- **`mouse_follows_focus` warps on *every* focus change**, not only focus changes rift itself caused — cmd-tab, a click in another app, an app raising its own window — matching Yabai's `WINDOW_FOCUSED` handler. Skipped when the pointer is already inside the window, during drags/workspace switches, under Mission Control, and during post-wake loginwindow replays.
- **`next_window` / `prev_window` re-scoped to stacks only.** Upstream cycled every window in the workspace; these now stand in for Yabai's `--focus stack.next` and do nothing outside a stack.
- **Dropping a dragged window no longer always swaps.** The target is divided Yabai-style: the middle swaps, the four edge zones insert-and-split (bsp only; other layouts still swap). The drop target is the tiled window under the *pointer*, not the window the dragged frame overlaps most.
- **`rebalance` semantics changed.** Previously (nominally) reset every split to 0.5 — three windows got 50/25/25. Now weights splits by leaves along the axis, like Yabai's equalize, and is actually invocable (`layout balance`).
- **Floats follow their live frames.** A visible float is laid out where the window server says it is; stored frames apply only to parked windows or placements rift itself intends (and that intent expires after 1.5s or first app response). A drop is "an observation, not an intent."
- **A manual float/tile toggle now outranks app rules.** Rules only set the default for windows the user hasn't decided about — rule semantics change from "enforced" to "initial".
- **Untracked/unadmitted windows are no longer remapped onto siblings.** Focus stays on the real window and focused-window commands are no-ops until it's admitted.
- **New (optional) dependency on Yabai's scripting addition** for moving windows between macOS spaces, creating/destroying spaces, instant space switching, and display-layout restore. All uses degrade gracefully when it's absent.
- **Always-on flight recorder**: rift permanently records events into a 32MB in-memory ring buffer, dumpable retroactively via `rift-cli execute trace dump`.
- **`restore_display_layouts = true` by default** — on display disconnect the layout is archived by display UUID and restored on return; displaced windows float over the survivor (`displaced_windows = "float" | "tile"`).
- **Default log level is `info`** unless `RUST_LOG` is set.
- **Adobe `AXLayoutArea` app rules removed from the dotfiles** — they only ever matched Lightroom's panels, which are now correctly refused at admission.

### Added

- **Real macOS space management** (macOS 26-verified, via the scripting-addition client `sys::scripting_addition`):
  - `switch_to_space <n>` — jump to a space by 1-based index, animation-free (`rift-cli execute space switch-to <n>`).
  - `move_window_to_space` (optionally following), `create_space` (inserted *next to* the active space), `destroy_space`.
  - `space_switch_method = "gesture" | "addition" | "auto"` — "auto" tries the ~0.3ms addition path, verifies the space actually changed, falls back to the synthetic dock swipe, and benches a misbehaving addition for 30s.
- **Drag-and-drop system**:
  - Drop overlay showing exactly where a dragged window will land (whole target for swap, the post-split half for an edge insert) — Liquid Glass (`NSGlassEffectView`), animated, configurable under `[settings.ui.drop_overlay]`, off by default, tiled-on-tiled only.
  - Edge-drop insert/split (see breaking changes).
  - Cross-display drops: preview and perform the split onto another display's tree; the drop lands on the display under the pointer.
  - Seam-drop policy: macOS relocates windows dropped straddling a display seam; rift watches and finishes the drop deterministically on the pointer's display. Opt-in `mouse.takeover_float_drags` lets rift consume float title-bar drags entirely (at the cost of tab tear-off).
- **Modifier-drag move/resize** (`[settings.mouse]`: `modifier`, `action1`, `action2`). Moves/resizes floats directly; resizes tiled windows through their split ratios; resize moves the edges the press landed on, throttled at Yabai's cadence. Right-button drags resolve the window via the window server.
- **Display-layout restore across unplug/sleep** (`restore_display_layouts`, `displaced_windows`), including putting native-fullscreen windows back into their old slot (or beside their old neighbour) on exit.
- **Layout commands**: `rotate = "90"|"180"|"270"`, `mirror = "x"|"y"` (tree layouts only), `balance`, stack cycling, `toggle_workspace_layout` (cycle the active workspace through a configured list of modes), `destroy_workspace` (rehomes windows to a neighbour first; never destroys the last workspace).
- **`directional_focus_skips_floating`** (off by default) — with it on, directional focus only ever walks the tiling tree, as Yabai's did.
- **`move_across_displays`** — directional focus/swap continue onto the neighbouring display (on by default).
- **`mouse_follows_focus_blacklist`** — bundle ids the cursor is never warped onto (screenshot/overlay tools); focus is unaffected, only the warp is suppressed.
- **Key names for every key code** — Backspace, the numpad block, CapsLock, volume keys, ContextMenu, etc. are now bindable; `KeyCode::ALL` + exhaustive round-trip test.
- **CLI additions**: `layout balance`, `layout next-window` / `prev-window`, `space switch-to <n>`, `workspace destroy`, `trace start/stop/dump`.
- **Trace/replay harness**: `rift-cli trace start/stop` records config, engine snapshot, window store, every event, out-of-band system answer, and frame write; `recorded_traces_replay_cleanly` replays `tests/traces/*.trace` against layout invariants (no writes to dragged windows, no empty frames, one tree per window, etc.). Open-loop: a replay only validates up to the first divergence. Plus the always-on flight recorder (see above).
- Scripting addition mocked under `cfg(test)`; cursor-location override and warp recorder for tests.

### Changed

- Focus resolution for same-app windows across displays: resolved via one z-ordered query across every display's current space, so clicking between two windows of one app on different displays no longer acts one click behind.
- `focus_display` / `move_mouse_to_display` now move real macOS keyboard focus (same `RaiseRequest` path as focus commands), and `focus_display` warps the cursor — without it, `focus_follows_mouse` immediately stole focus back (fixes #332).
- `move_node` warps the cursor with the window (deferred to the post-layout frame), routed through the per-app `mouse_follows_focus` blacklist.
- Discovery sweeps are deferred during drags (a cross-seam drag used to trigger ~11 full every-app AX censuses per second); the owed sweep flushes on mouse-up.
- Space switching on macOS 26 rewritten to follow joshuarli/iss (three gesture phases, session tap, paired companion events) — the previous synthetic swipe silently did nothing on 26.6.2.
- `tests/traces/` pruned from 25MB to ~4MB (superseded recordings remain in git history).

### Fixed

- **Hotkeys**: 85 key codes (including letters I J K L M N O P U) fell into a `Display` catch-all, rendered as "Other", failed to re-parse, and were silently never registered. Every variant now survives the Display → FromStr round trip.
- **Dock orientation constants were wrong** (left/right misclassified): a left-positioned dock made tiled windows start underneath it and stop short of the far edge.
- **App rules vs. manual tiling** (three chained bugs): catch-all `floating = true` rules re-floated hand-tiled windows on every space activation; rule-floated windows stayed in the tree; re-adding a window stranded orphan leaves (a live tree held five leaves for two windows).
- **Closed-but-ordered-out windows** (apps that keep running) are untiled immediately: CGS hide events reach the reactor and the window's AX element — the only reliable oracle — settles whether it's really gone. (An earlier SLSWindowQuery-based attempt was reverted as unsound.)
- **Drag correctness**: a refused insert no longer strands the dragged window out of every tree; a drop with no swap still triggers the arrange it owes; the overlay can no longer linger after any drag-ending path; splits are previewed on a tree copy so the promise matches the drop.
- **Modifier resize**: edges no longer run backwards (edge selection at mouse-down from press position); no drift (deltas measured from the press, applied to the captured frame); boundary drags move the split that owns the boundary, not the nearest one; windows that refuse a size (Finder's unpublished minimum) get their minimum recorded and the neighbour gives way; clamping stops inverted windows walking away.
- **Fullscreen detection**: only transitions *across* the full-screen boundary count as self-fullscreen — a resize of an already-screen-filling window is an ordinary resize again (#415 regression); non-native fullscreen self-resizes no longer rewrite split ratios (AeroSpace's model).
- **Focus/admission**: focus reported on non-standard AX child windows (Lightroom panels) resolves to the managed top-level window — but only for genuinely tracked child windows, never by remapping an unadmitted top-level window onto a sibling (Preview's second document got its sibling toggled); rejected windows are re-judged on activation with a live layer query; unadmitted windows are refused at every layout-event path, not just discovery.
- **Displays/spaces**: a display's space listed anywhere is never remapped (no more layout-less stranded desktops); a shown space with no layout is always exposed; `save-layout` fixed on machines with spare desktops; cross-display drags hold the window instead of re-tiling it under the cursor; recent pending frame writes are believed over contradicting space reports (no more cross-display chases).
- **Floats**: silenced AX move/resize notifications during float drags (Warp's tab-bar drag glitched); a dragged float's space membership follows the display under it; a float on another display is "elsewhere", not "parked" (no phantom restores); floats with no stored frame are laid out where they are, not centered.
- **Pointer warps**: no warp onto a window the pointer is already inside (stack stepping dragged the cursor repeatedly); no warp on pointer-driven focus changes or focus returning from windowless apps.
- Drop overlay early fixes: y-flip into layer coordinates; frames scheduled on the dispatch queue instead of a Tokio timer (which panicked and put rift in a launchd restart loop).

### Removed

- Adobe `AXLayoutArea` rules from the dotfiles (only ever matched panels rift now refuses to admit).
- Superseded trace fixtures (kept in git history at `bc57386`).

</details>
