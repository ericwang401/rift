# Changelog

All notable changes in this fork ([ericwang401/rift](https://github.com/ericwang401/rift)) relative to upstream [acsandmann/rift](https://github.com/acsandmann/rift), diverging at `73a475f`. 47 commits, ~42,600 insertions across 114 files.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### ⚠ Breaking / intent-diverging changes

These do more than fix bugs — they change behavior upstream users may rely on, or steer the project in a direction upstream did not intend (broadly: toward yabai parity, real macOS spaces, and heavier instrumentation).

- **`mouse_follows_focus` warps on *every* focus change**, not only focus changes rift itself caused — cmd-tab, a click in another app, an app raising its own window, floating or tiled — matching yabai's `WINDOW_FOCUSED` handler. Skipped when the pointer is already inside the window, during drags/workspace switches, under Mission Control, and during post-wake loginwindow replays. Upstream's quieter warp behavior is gone.
- **`next_window` / `prev_window` re-scoped to stacks only.** Upstream cycled every window in the workspace; these now stand in for yabai's `--focus stack.next` and do nothing outside a stack. Users who used them as a general window cycler lose that.
- **Dropping a dragged window no longer always swaps.** The target is divided yabai-style: the middle swaps, the four edge zones insert-and-split (bsp only; other layouts still swap). The drop target is now the tiled window under the *pointer*, not the window the dragged frame overlaps most.
- **`rebalance` semantics changed.** Previously (nominally) reset every split to 0.5 — three windows got 50/25/25. Now weights splits by leaves along the axis, like yabai's equalize, and is actually invocable (`layout balance`).
- **Floats follow their live frames.** A visible float is laid out where the window server says it is; stored frames apply only to parked windows or placements rift itself intends (and that intent expires after 1.5s or first app response). Upstream's stored-frame enforcement — which snapped floats back after drags and re-centered them — is intentionally abandoned. A drop is "an observation, not an intent."
- **A manual float/tile toggle now outranks app rules.** Upstream re-applied `floating = true` rules on every space activation, undoing manual tiling; rules now only set the default for windows the user hasn't decided about. Rule semantics change from "enforced" to "initial".
- **Untracked/unadmitted windows are no longer remapped onto siblings.** Focus stays on the real window and focused-window commands are no-ops until it's admitted — commands can now silently do nothing where they previously acted (on the *wrong* window).
- **New dependency on yabai's scripting addition (code injected into Dock)** for moving windows between macOS spaces, creating/destroying spaces, instant space switching, and display-layout restore. All uses degrade gracefully when absent (commands report failure; default space switch stays the unprivileged gesture), but this embraces an elevated-privilege mechanism upstream did not use.
- **Always-on flight recorder**: rift permanently records events into a 32MB in-memory ring buffer, dumpable retroactively via `rift-cli execute trace dump`. Constant memory overhead by design.
- **`restore_display_layouts = true` by default** — on display disconnect (unplug/sleep/lid close) the layout is archived by display UUID and restored on return; displaced windows float over the survivor (`displaced_windows = "float" | "tile"`). New default-on behavior that moves windows across spaces via the scripting addition.
- **Default log level is `info`** unless `RUST_LOG` is set.
- **Adobe `AXLayoutArea` app rules removed from the dotfiles** — in rift they only ever matched Lightroom's panels, which are now correctly refused at admission.
- **Overall direction**: many behaviors were checked against yabai's source and deliberately made to match (directional focus, drop zones, resize edges, balance weighting, cursor centering, space commands). Where upstream rift and yabai disagreed, this fork generally sides with yabai.

### Added

- **Real macOS space management** (macOS 26-verified, via the scripting-addition client `sys::scripting_addition`):
  - `switch_to_space <n>` — jump to a space by 1-based index, animation-free (`rift-cli execute space switch-to <n>`).
  - `move_window_to_space` (optionally following), `create_space` (inserted *next to* the active space, not appended), `destroy_space`.
  - `space_switch_method = "gesture" | "addition" | "auto"` — "auto" tries the ~0.3ms addition path, verifies the space actually changed, falls back to the synthetic dock swipe, and benches a misbehaving addition for 30s.
- **Drag-and-drop system**:
  - Drop overlay showing exactly where a dragged window will land (whole target for swap, the post-split half for an edge insert) — Liquid Glass (`NSGlassEffectView`), animated, configurable under `[settings.ui.drop_overlay]`, off by default, tiled-on-tiled only.
  - Edge-drop insert/split (see breaking changes).
  - Cross-display drops: preview and perform the split onto another display's tree; the drop lands on the display under the pointer.
  - Seam-drop policy: macOS relocates windows dropped straddling a display seam; rift watches and finishes the drop deterministically on the pointer's display. Opt-in `mouse.takeover_float_drags` lets rift consume float title-bar drags entirely (at the cost of tab tear-off).
- **Modifier-drag move/resize** (`[settings.mouse]`: `modifier`, `action1`, `action2`) — yabai's `mouse_modifier`/`mouse_action1`/`mouse_action2`. Moves/resizes floats directly; resizes tiled windows through their split ratios; resize moves the edges the press landed on, throttled at yabai's cadence. Right-button drags resolve the window via the window server (the CGEvent field is empty for them).
- **Display-layout restore across unplug/sleep** (`restore_display_layouts`, `displaced_windows`), including putting native-fullscreen windows back into their old slot (or beside their old neighbour) on exit.
- **Layout commands**: `rotate = "90"|"180"|"270"`, `mirror = "x"|"y"` (tree layouts only), `balance`, stack cycling, `toggle_workspace_layout` (cycle the active workspace through a configured list of modes), `destroy_workspace` (rehomes windows to a neighbour first; never destroys the last workspace).
- **`directional_focus_skips_floating`** (off by default) — with it on, alt-hjkl behaves as yabai's did: directional focus only ever walks the tiling tree.
- **`move_across_displays`** — directional focus/swap continue onto the neighbouring display (on by default).
- **`mouse_follows_focus_blacklist`** — bundle ids the cursor is never warped onto (screenshot/overlay tools); focus is unaffected, only the warp is suppressed.
- **Key names for every key code** — Backspace, the numpad block, CapsLock, volume keys, ContextMenu, etc. are now bindable; `KeyCode::ALL` + exhaustive round-trip test.
- **CLI additions**: `layout balance`, `layout next-window` / `prev-window`, `space switch-to <n>`, `workspace destroy`, `trace start/stop/dump`.
- **Trace/replay harness**: `rift-cli trace start/stop` records config, engine snapshot, window store, every event, out-of-band system answer, and frame write; `recorded_traces_replay_cleanly` replays `tests/traces/*.trace` against layout invariants (no writes to dragged windows, no empty frames, one tree per window, etc.). Open-loop: a replay only validates up to the first divergence. Plus the always-on flight recorder (see breaking changes).
- Scripting addition mocked under `cfg(test)`; cursor-location override and warp recorder for tests.

### Changed

- Focus resolution for same-app windows across displays: resolved via one z-ordered query across every display's current space, so clicking between two windows of one app on different displays no longer acts one click behind.
- `focus_display` / `move_mouse_to_display` now move real macOS keyboard focus (same `RaiseRequest` path as focus commands), and `focus_display` warps the cursor — without it, `focus_follows_mouse` immediately stole focus back (fixes upstream #332).
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
- **Fullscreen detection**: only transitions *across* the full-screen boundary count as self-fullscreen — a resize of an already-screen-filling window is an ordinary resize again (upstream #415 regression); non-native fullscreen self-resizes no longer rewrite split ratios (AeroSpace's model).
- **Focus/admission**: focus reported on non-standard AX child windows (Lightroom panels) resolves to the managed top-level window — but only for genuinely tracked child windows, never by remapping an unadmitted top-level window onto a sibling (Preview's second document got its sibling toggled); rejected windows are re-judged on activation with a live layer query; unadmitted windows are refused at every layout-event path, not just discovery.
- **Displays/spaces**: a display's space listed anywhere is never remapped (no more layout-less stranded desktops); a shown space with no layout is always exposed; `save-layout` fixed on machines with spare desktops; cross-display drags hold the window instead of re-tiling it under the cursor; recent pending frame writes are believed over contradicting space reports (no more cross-display chases).
- **Floats**: silenced AX move/resize notifications during float drags (Warp's tab-bar drag glitched); a dragged float's space membership follows the display under it; a float on another display is "elsewhere", not "parked" (no phantom restores); floats with no stored frame are laid out where they are, not centered.
- **Pointer warps**: no warp onto a window the pointer is already inside (stack stepping dragged the cursor repeatedly); no warp on pointer-driven focus changes or focus returning from windowless apps.
- Drop overlay early fixes: y-flip into layer coordinates; frames scheduled on the dispatch queue instead of a Tokio timer (which panicked and put rift in a launchd restart loop).

### Removed

- Adobe `AXLayoutArea` rules from the dotfiles (only ever matched panels rift now refuses to admit).
- Superseded trace fixtures (kept in git history at `bc57386`).
