# Releasing Prompter

Pushing a `v*` tag runs `.github/workflows/release-macos.yml`, which:

1. Builds the Swift speech helper and the Tauri app.
2. Signs both with the Developer ID (helper first, then the app) using the hardened runtime and `crates/app/Entitlements.plist`.
3. Notarizes and staples the app.
4. Packages a DMG, then signs, notarizes and staples it.
5. Signs the updater bundle (`Prompter.app.tar.gz`) with the Prompter updater key.
6. Uploads the DMG, the updater bundle, its signature and `latest.json` (the in-app updater's feed) to the tag's GitHub release, creating the release if it doesn't exist.

## Cutting a release

1. Bump the version in `Cargo.toml` (`[workspace.package]`) and `crates/app/tauri.conf.json`. The workflow refuses a tag that doesn't match both.
2. Commit, then tag and push: `git tag -a v0.2.1 -m "Prompter v0.2.1" && git push origin master v0.2.1`.
3. Optionally create the GitHub release with notes first. Otherwise the workflow creates one with generated notes, which you can edit afterwards. The release body becomes the update notes in `latest.json`.

To rebuild an existing tag, run the workflow manually (Actions, then Release macOS, then Run workflow) with the tag.

## Secrets

Repository secrets (Settings, then Secrets and variables, then Actions):

| Secret | What |
|---|---|
| `APPLE_CERTIFICATE` | Developer ID Application certificate (.p12), base64 |
| `APPLE_CERTIFICATE_PASSWORD` | Its password |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Name (TEAMID)` |
| `APPLE_API_ISSUER` | App Store Connect API issuer ID (notarization) |
| `APPLE_API_KEY` | App Store Connect API key ID |
| `APPLE_API_PRIVATE_KEY` | Contents of the `AuthKey_<id>.p8` file |
| `TAURI_SIGNING_PRIVATE_KEY` | Prompter updater private key |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Its password |

The Apple values are the same ones the Minutes release uses.

## The updater key

Installed copies trust updates signed by the key whose public half is in `crates/app/tauri.conf.json` (`plugins.updater.pubkey`). The private key and its password were generated on 2026-09-23 and live in `~/.tauri/prompter-updater.key` and `~/.tauri/prompter-updater.key.password` on Mat's MacBook, as well as in the repository secrets above. Keep a backup: if the key is lost, installed copies can no longer be updated, and everyone has to reinstall once from a DMG signed with a new key.

## Local builds

`./scripts/build.sh` builds a locally signed (ad hoc) app with the same hardened runtime and entitlements as a release, and `./scripts/make-dmg.sh <app> <out.dmg>` packages it. Neither build is notarized, so another Mac will warn on first launch.
