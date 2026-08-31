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

### Deprecated

- **`rift-cli` is deprecated** in favour of `rift`, which now takes the same
  subcommands. It still works and still ships; the documentation and the
  `run_on_start` examples in `rift.default.toml` now say `rift`. Run by hand it
  prints a deprecation notice, and only then — the notice is suppressed unless
  stderr is a terminal, so `run_on_start`, `subscribe cli` and hotkeys stay
  silent.

### Fixed

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
