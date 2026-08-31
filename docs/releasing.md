# Releasing

Releases are produced by [`.github/workflows/release.yml`](../.github/workflows/release.yml)
when a version tag is pushed. You should never need to build one by hand.

> One-time setup — Apple Developer account, GitHub secrets — lives in
> [ci-setup.md](ci-setup.md). Read that first if releases have never run here.

## TL;DR

```sh
just bump 0.5.3-plus.2          # Cargo.toml + Cargo.lock, after confirming
$EDITOR CHANGELOG.md            # move [Unreleased] under the new version
git commit -am "chore(release): 0.5.3-plus.2"
just tag                        # annotated tag from Cargo.toml, with guards
git push origin main --follow-tags
```

Pushing the tag builds, signs, notarizes, and opens a **draft** release. Review
it in the Releases tab and publish; publishing bumps the Homebrew formula.

## Versioning

Semver with a fork suffix: `<upstream>-plus.<n>`.

```
0.5.3-plus.1, 0.5.3-plus.2, then 0.6.0-plus.1 after rebasing onto upstream v0.6.0
```

`-plus.n` is a semver prerelease identifier, so it is valid and orders correctly
within the fork's own line. It sorts *below* a bare `0.5.3`, which never matters
because `rift-plus` is its own formula, separate from upstream's `rift`.

`Cargo.toml` is the source of truth. The tag is `v` + that version.

## The recipes

| recipe | what it does |
|---|---|
| `just bump <version>` | rewrites `Cargo.toml`, refreshes `Cargo.lock`, after showing both and confirming |
| `just tag` | annotated tag from `Cargo.toml`'s version |
| `just untag <version>` | deletes the tag locally and on origin, and offers to delete its release |

Each guards the mistakes that are easy to make and annoying to undo:

- **Bare versions only.** `just bump v1.2.3` is refused, because the `v` is
  added when tagging and `vv1.2.3` is a tag nobody wants to chase.
- **Malformed semver is refused**, against semver.org's own grammar.
- **`bump` is idempotent.** Already at the target with a matching lockfile? It
  says so and stops. `Cargo.toml` bumped but `Cargo.lock` left behind — the
  usual state after a half-finished attempt — is detected and finished, rather
  than bumped a second time.
- **`tag` is annotated**, so `git push --follow-tags` carries it. It refuses a
  dirty tree, a lockfile that disagrees with `Cargo.toml`, a missing changelog
  section, and a tag that already exists.
- **`untag` confirms first**, then checks for a release on that tag and *asks*
  rather than assuming. The release action upserts, so leaving it is the right
  answer when you are about to re-tag the same version.

## What the workflow does

On a `v*` tag push, a `macos-14` runner:

1. **Validates the tag** as semver, with a specific error for a doubled `v`.
2. **Checks the metadata agrees**: `Cargo.toml`, the `rift-wm` entry in
   `Cargo.lock`, and a `## [<version>]` heading in `CHANGELOG.md`. A bumped
   `Cargo.toml` with a stale lockfile means the bump was never committed
   properly, so it fails here rather than shipping.
3. **Imports** the Developer ID certificate into a throwaway keychain.
4. **Builds** `aarch64` and `x86_64`, and `lipo`s them universal. Not stripped —
   the release profile keeps debuginfo so crash reports symbolicate.
5. **Codesigns** both binaries with the hardened runtime and a secure timestamp.
6. **Notarizes** with `xcrun notarytool submit --wait` and an App Store Connect
   API key.
7. **Assembles** `rift-plus-universal-macos-<version>.tar.gz` (both binaries plus
   `rift.default.toml`) and a `.sha256` beside it.
8. **Extracts the release notes** from the matching `CHANGELOG.md` section.
9. **Opens a draft release** with both files attached.

Publishing the draft fires [`tap.yml`](../.github/workflows/tap.yml), which
verifies the checksum file against the tarball and bumps `Formula/rift-plus.rb`
in [performave/homebrew-tap](https://github.com/performave/homebrew-tap).

### Why the tap bump is a separate workflow

A draft release's asset URLs 404 for everyone but you. Bumping the formula at
build time would point `brew` at a download that does not exist yet, so the bump
waits for the release to actually be published.

## Signing and notarization

Release binaries are Developer ID signed **and notarized in CI**. This is not
cosmetic: macOS records the Accessibility grant against a binary's designated
requirement, and an ad-hoc signature pins that to a cdhash which changes on
every build — so an ad-hoc rift loses Accessibility on every upgrade and
respawns in a launchd loop. A Developer ID signature gives a stable requirement
that survives upgrades, and notarization keeps Gatekeeper quiet on a machine
that did not build it.

The one thing never to hardened-sign is the scripting addition's loader and
payload: they are written to `/Library/ScriptingAdditions` and ad-hoc signed at
`rift sa load` time, because they run inside Dock. Signing the main binary does
not affect them.

## If something fails

| symptom | cause |
|---|---|
| "tag has a doubled 'v' prefix" | `just tag` adds the `v`; pass bare versions to `bump`/`untag` |
| "Cargo.lock says X, tag says Y" | the bump was not committed — `just bump <version>` again, then commit |
| "CHANGELOG.md has no section" | release notes come from it; add the heading |
| cert import fails | bad `APPLE_CERTIFICATE` base64 or wrong password — see [ci-setup.md](ci-setup.md) |
| notarytool `Invalid` | binary not signed with `--options runtime` / `--timestamp`, or not a Developer ID cert |
| tap not bumped | `HOMEBREW_TAP_TOKEN` missing; the workflow warns and skips. Edit `version` and `sha256` in the formula by hand |
