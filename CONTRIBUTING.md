# Contributing

This is a personal fork of [acsandmann/rift](https://github.com/acsandmann/rift).
Upstream is where general rift improvements belong; what lives here is the set of
patches this fork carries on top, kept small and focused so rebasing onto a new
upstream release stays cheap.

Start with **[docs/development.md](docs/development.md)** for the build and the
dev loop.

## Commit messages

All commits use [Conventional Commits](https://www.conventionalcommits.org/):

```text
<type>(<scope>): <summary>
<type>: <summary>
```

Imperative, lower case, no trailing period:

```text
feat(space): move windows between spaces, and create/destroy them
fix(focus): a Dock click warps the pointer to the window it summons
docs: document the scripting addition
```

Preferred types are `feat`, `fix`, `docs`, `build`, `ci`, `perf`, `refactor`,
`test` and `chore`. Mark a breaking change with `!` after the type or scope, or
a `BREAKING CHANGE:` footer.

## Changelog

[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Add anything
user-visible to the `## [Unreleased]` section of `CHANGELOG.md` in the same
commit as the change — not at release time, when the reasoning has faded.

Entries describe this fork's behaviour **relative to upstream**, since that is
the difference a reader is here to understand. Group them under `Added`,
`Changed`, `Deprecated`, `Removed`, `Fixed` or `Security`.

Release notes are extracted from that file verbatim, so write the entry you
would want to read on the release page.

## Versioning

[Semantic Versioning](https://semver.org/spec/v2.0.0.html), with the fork suffix
`<upstream>-plus.<n>` — `0.5.3-plus.1`, then `0.5.3-plus.2`, then
`0.6.0-plus.1` after rebasing onto upstream `v0.6.0`. See
[docs/releasing.md](docs/releasing.md).

## Documentation

Prose belongs in `docs/`. Keep `README.md` lean — what rift is, how to install
it, and links onward. A README that grows into a manual stops being read.

## Code

- Match the surrounding style. Comments explain *why*, in prose, at the density
  of the code around them.
- `just fmt` formats the files you changed. **Never** `cargo fmt --all`: the
  tree inherited from upstream does not satisfy current nightly rustfmt, and
  reformatting it wholesale makes every future rebase painful. CI only checks
  files a change actually touched.
- `just check` runs what CI runs.

## Branches

`main` is pushed to directly; there is no ruleset requiring pull requests, since
this is a single-maintainer fork and rebases onto upstream would fight one. CI
still runs on every push, and a red `test` workflow is a real failure. If this
ever becomes a shared repository, requiring a PR and a green `test` check on
`main` is the change to make.
