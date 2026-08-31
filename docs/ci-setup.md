# CI setup

One-time setup before the [`release` workflow](../.github/workflows/release.yml)
can build signed, notarized releases. Once these secrets exist, releasing is
just [pushing a tag](releasing.md).

**The secret names match the `yabai-plus` repo on purpose** — same Apple
account, same tap org, so the same six values work for both. If you have already
set them up there, copy them across and skip to [Verify](#verify).

## Prerequisites

- An **Apple Developer Program** membership — needed for a Developer ID
  Application certificate and for notarization.
- Admin access to this repo, to add Actions secrets.

## 1. Developer ID Application certificate

1. In **Keychain Access** (or Xcode → Settings → Accounts → Manage
   Certificates), create or download a **Developer ID Application** certificate.
2. Export it as `.p12`: select the certificate **and its private key** →
   right-click → Export, and set an export password.
3. Note the exact identity string:
   ```sh
   security find-identity -v -p codesigning
   # -> "Developer ID Application: Eric Wang (8UR4G77744)"
   ```

## 2. App Store Connect API key

Used for notarization. An API key avoids the 2FA and session flakiness of
Apple-ID plus app-specific passwords.

1. <https://appstoreconnect.apple.com> → **Users and Access** → **Integrations**
   → **App Store Connect API** → generate a key with the **Developer** role.
2. Record the **Key ID** and **Issuer ID**.
3. Download `AuthKey_XXXXXXXX.p8`. **It can only be downloaded once.**

## 3. Secrets

Under **Settings → Secrets and variables → Actions**:

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | base64 of the `.p12`: `base64 -i cert.p12 \| pbcopy` |
| `APPLE_CERTIFICATE_PASSWORD` | the password set when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | the full identity, e.g. `Developer ID Application: Eric Wang (8UR4G77744)` |
| `APPLE_API_KEY` | App Store Connect **Key ID** |
| `APPLE_API_ISSUER` | App Store Connect **Issuer ID** (a UUID) |
| `APPLE_API_PRIVATE_KEY` | the raw contents of `AuthKey_XXXX.p8`, including the BEGIN/END lines |
| `HOMEBREW_TAP_TOKEN` | a token with `contents: write` on `performave/homebrew-tap` |

Scripted, rather than clicking:

```sh
gh secret set APPLE_CERTIFICATE          < <(base64 -i cert.p12)
gh secret set APPLE_CERTIFICATE_PASSWORD --body 'the-p12-password'
gh secret set APPLE_SIGNING_IDENTITY     --body 'Developer ID Application: Eric Wang (8UR4G77744)'
gh secret set APPLE_API_KEY              --body 'ABC123DEF4'
gh secret set APPLE_API_ISSUER           --body '00000000-0000-0000-0000-000000000000'
gh secret set APPLE_API_PRIVATE_KEY      < AuthKey_ABC123DEF4.p8
gh secret set HOMEBREW_TAP_TOKEN         --body 'ghp_...'
```

`HOMEBREW_TAP_TOKEN` is the only optional one: without it the release still
builds and publishes, and only the formula bump is skipped, with a warning
saying so.

## Why these choices

These mirror a setup already known to work, and sidestep the usual notarization
failures:

- **`apple-actions/import-codesign-certs`** for the import — it creates a
  temporary keychain and sets the key partition list so `codesign` works
  non-interactively, which is the common failure when done by hand on recent
  runners.
- **An App Store Connect API key** rather than Apple-ID auth — no 2FA in CI.
- **The `.p8` stored as raw text**, written with `printf`, so no base64
  round-trip can mangle it.

## Verify

1. `gh secret list` shows all seven.
2. Trigger a run: push a `v*` tag, or use **workflow_dispatch** on the Actions
   tab with a tag that already exists.
3. Watch **Notarize** — `notarytool ... --wait` blocks until Apple answers
   `Accepted`, or rejects with a log URL.

## Troubleshooting

| Symptom | Likely cause |
| --- | --- |
| cert import fails | bad `APPLE_CERTIFICATE` base64 (re-export, use `base64 -i`) or wrong `APPLE_CERTIFICATE_PASSWORD` |
| `codesign`: no identity found | `APPLE_SIGNING_IDENTITY` does not match `security find-identity` output exactly |
| notarytool `Invalid` | not signed with `--options runtime` / `--timestamp`, or signed with a non-Developer-ID cert |
| notarytool auth error | wrong Key ID or Issuer ID, or truncated `.p8` text |
| release not created | tag did not match `v*`, or `contents: write` is missing |
| formula not bumped | `HOMEBREW_TAP_TOKEN` missing or lacking write access to the tap |
