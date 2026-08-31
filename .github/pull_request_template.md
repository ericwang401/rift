## What and why

<!-- What changes, and the reason it is worth changing. Link an issue if there is one. -->

## How it was verified

<!-- What you actually ran or observed. "Builds" is not verification for a window
     manager: say which behaviour you exercised, on which display setup. -->

- [ ] `just check` passes
- [ ] Tried it against a running rift (`just dev`)

## Checklist

- [ ] Commits follow [Conventional Commits](https://www.conventionalcommits.org/)
- [ ] `CHANGELOG.md` has an entry under `## [Unreleased]`, if this is user-visible
- [ ] `just fmt` run on changed files (never `cargo fmt --all`)
- [ ] Prose went in `docs/`, not the README

<!-- Touching src/osax/ or src/sys/osax.rs? Read docs/scripting-addition.md
     first, and keep OSAX_VERSION in step across the C and Rust definitions. -->
