# Changelog

All notable changes to this fork are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
with the fork scheme described in [AGENTS.md](AGENTS.md): `<upstream>-plus.<n>`.

Entries describe this fork's changes relative to
[upstream rift](https://github.com/acsandmann/rift), not upstream's own history.

## [Unreleased]

### Added

- **`rift` now accepts every `rift-cli` subcommand**: `rift query windows`,
  `rift execute …` and `rift subscribe …` work exactly as their `rift-cli`
  spellings do, so the one binary covers running the window manager, managing
  the service and the scripting addition, and driving a running instance.
  `rift-cli` keeps working unchanged — it is a second entry point into the same
  code, not a second implementation.
- **`rift status`** reports whether the window manager is running, whether
  launchd is keeping it alive, and whether the scripting addition inside Dock is
  loaded and healthy — each probed separately so the output says which one to
  fix. It round-trips a real query rather than only looking the Mach service up,
  so "running but not answering" is distinguishable from "not running". `--json`
  for scripts; the exit status follows the window manager alone, since rift runs
  without the scripting addition.

- **The layout survives a restart of rift.** rift could always write its layout
  and start from a written one, but only when told to by hand: nothing saved on
  the way out and nothing read on the way in, so every crash, `brew services
  restart` or rebuild dropped back to the app rules — under a float-by-default
  config, everything to be re-tiled. It now saves on SIGTERM and on a heartbeat
  (a crash and a `kill -9` reach neither the handler nor a manual save), and
  reads the file back at startup.

  The restore is deliberately conditional. A snapshot is only worth putting back
  if rift is coming straight back up; after a reboot or an afternoon away the
  windows have moved on without it, and reasserting a stale arrangement would
  fight the user rather than help. `max_age_secs` bounds how old a snapshot may
  be, measured from when it was written — which with the heartbeat running is
  the last moment rift was known to be alive, so the age is the length of the
  gap. `rift --restore` still restores by hand, ignoring the age.

  Restoring also records the snapshot's tiled/floating verdict as the user's own
  choice, the same standing a manual toggle has. Without that the restore held
  only until the next space activation re-ran the app rules and a catch-all
  `floating` rule floated everything back. Off by default; see
  `[settings.layout_restore]` in `rift.default.toml`.

### Deprecated

- **`rift-cli` is deprecated** in favour of `rift`, which now takes the same
  subcommands. It still works and still ships; the documentation and the
  `run_on_start` examples in `rift.default.toml` now say `rift`. Run by hand it
  prints a deprecation notice, and only then — the notice is suppressed unless
  stderr is a terminal, so `run_on_start`, `subscribe cli` and hotkeys stay
  silent.

### Fixed

- **A window that goes native fullscreen and comes back lands where it was**,
  not beside whatever is selected, and without passing through the wrong place
  on the way. A browser tiled left of an editor came back on the right after a
  video was fullscreened and closed — or, in a layout of stacked columns, on top
  of the editor. rift already remembered the window's slot on the way out, but
  the transition itself defeated it at every step: the slot was read from a
  workspace assignment macOS clears first, so usually nothing was recorded;
  when something was, it was recorded on a later removal, after the transition
  had already shoved the window across; the exit handler and a transient
  "not admitted" removal each threw the slot away; and the window was put back
  into the tree by whichever of several paths noticed first, none of which
  consulted it. Now the slot is captured at the first removal with the space
  read off the tree, survives until the window is really gone, and is
  reinstated by whatever event actually re-inserts the window — before that
  event writes a frame. The whole layout is restored as it was; the "beside
  its old neighbour" fallback is used only when the snapshot matches nothing,
  since splitting a stacked neighbour puts the window into the stack.
- **The drop overlay no longer promises a move a stack cannot make.** Dragging
  a window on a space in stack mode drew a screen-sized drop region for the
  length of the drag, and releasing it swapped the dragged window with an
  arbitrary member of the stack — a change nothing on screen reflects, since a
  stack hands every window the same rect and shows one at a time. Windows in
  one stack are no longer offered to each other as drop targets, so the drag
  shows nothing and does nothing. The same holds for a stacked container inside
  the traditional layout; drops between windows that really do occupy different
  places are untouched.
- `rift-cli service …` printed "service commands have been moved to the `rift`
  binary" and exited 0 without doing anything, so a script could not tell the
  difference between starting the service and not starting it. The subcommands
  work again.
- `rift service install` wrote a plist pointing at whichever `rift` came first
  on `$PATH` rather than the one being run, so `rift service start` from a dev
  build would install and restart Homebrew's rift instead.
- `rift status`'s launchd check looks for the Homebrew labels as well as rift's
  own, because `brew services` starts rift under `homebrew.mxcl.rift`, not the
  `git.acsandmann.rift` that `rift service` manages. Checking only the latter
  reports a perfectly healthy Homebrew install as "not installed".
- `just fmt` reformatted the whole crate whenever a change touched a module
  root such as `src/lib.rs`: rustfmt follows `mod` declarations, so naming one
  file pulled in every file below it — the wholesale reformat the recipe exists
  to prevent. It and the CI check now pass `--skip-children`.
- `rift-cli execute` reported `Command executed successfully` for every
  command, including the three that need the scripting addition and do nothing
  without it. They now print why they could not run and exit non-zero.
- `just dev` / `just install` ignored a `formula=` override, because they
  chained through nested `just` calls rather than dependencies — building one
  thing and installing into another.
- **A window taken into native fullscreen came back floating** — Discord
  fullscreening a video, or anything else that moves a window to a space of its
  own. From the space it left, such a window looks exactly like one that closed:
  it is no longer ordered in there. Rift retired it on that evidence alone,
  which destroyed its record and with it the manual float/tile choice that
  outranks a matching app rule, so the window returned as a stranger for the
  rules to place again. Departure now has to be corroborated by the window
  server having forgotten the id, which a window sitting in a fullscreen space
  has not.
- **A window came back from native fullscreen in the wrong slot** — tiled on
  the left, back on the right. The window server announces the transition
  twice, as a departure from the window's own space and an arrival on the
  fullscreen one, in either order. Rift only recorded the window's slot once it
  had seen the arrival, so whenever the departure landed first the slot was read
  after the window had already left its tree: no anchor and a snapshot that no
  longer held it, leaving it to come back beside whatever was selected. The slot
  is now taken from whichever removal still has one to take. A single tiled
  window on its space could not show this; two could.

### Changed

- **`WindowServerAppeared` and `WindowServerDestroyed` are recorded in traces.**
  They were `#[serde(skip)]`, so `rift execute trace dump` silently omitted
  them — and since a window's arrival on and departure from a space is where
  native fullscreen is decided, every bug in that area was invisible to the one
  tool meant to explain it.

## [0.5.3-plus.1] - 2026-08-31

First tagged release of the fork, against upstream `v0.5.3`.

### Added

- **A scripting addition of rift's own.** rift builds, installs and injects its
  own payload into Dock (`/Library/ScriptingAdditions/rift.osax`, serving
  `/tmp/rift-sa_$USER.socket`), so moving a window to a space, creating a space
  and destroying one no longer depend on yabai being installed. New commands:
  `rift sa status | load | install | uninstall | install-sudoers |
  uninstall-sudoers`. See [docs/scripting-addition.md](docs/scripting-addition.md).
- **Display layout restore.** A display's layout is remembered when it
  disconnects — unplug, sleep, lid close — and restored when the same display
  returns, including fullscreen windows. `displaced_windows` chooses whether a
  departed display's windows float over the survivor or join its tree.
- **Drag improvements.** Dropping on a window's edge splits it rather than only
  swapping; cross-display drops preview and perform the split; a drop overlay
  drawn in Liquid Glass shows where a dragged window will land.
- **Space commands.** Switch to a space by number instantly, move windows
  between spaces, create and destroy spaces, and toggle layout modes.
- **Layout commands.** Cycle through a stack and balance the tree; `rotate` and
  `mirror`; column/row ordering in the query API.
- **Modifier-drag.** Hold a modifier and drag anywhere in a window to move or
  resize it; resizing a tiled window adjusts its split ratios.
- **`manage = true`** lets nominally unmanageable windows into the layout, and
  a catch-all rule can make floating the default.
- **An always-on flight recorder** (`sys::trace`) capturing activity from every
  thread, with a replay harness for reproducing reported sequences.

### Fixed

- Focus follows the window a Dock click summons, rather than leaving the pointer
  behind.
- Focus resolves across all visible spaces for same-app windows, and is never
  remapped onto an untracked or unadmitted sibling.
- Floating windows stay where the user puts them across drags and seam drops.
- `mouse_follows_focus` no longer fights the pointer during a drag.
- The drop overlay checks for `NSGlassEffectView` before using it, so a
  system older than macOS 26 loses the overlay rather than the process.

### Changed

- Parity with yabai for directional focus, cross-display moves and space
  creation.
- The release profile ships unstripped, so crash reports symbolicate.

[Unreleased]: https://github.com/performave/rift-plus/compare/v0.5.3-plus.1...HEAD
[0.5.3-plus.1]: https://github.com/performave/rift-plus/compare/v0.5.3...v0.5.3-plus.1
