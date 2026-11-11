# AGENTS.md

Notes for coding agents working on LC Menubar, a macOS menu bar app for Laravel Cloud written in Rust. The whole project is vibe-coded with Claude; keep it that way: small, focused changes, verified before claiming done.

## Commands

```sh
cargo run --release -- --demo   # sample data, no Keychain or network
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
UNIVERSAL=0 scripts/bundle.sh   # local .app + zip in dist/
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy with `-D warnings` and tests on macOS. Run all three before finishing.

## Layout

| File | Purpose |
| --- | --- |
| `src/main.rs` | Event loop, tray, menu building, actions, demo data |
| `src/model.rs` | Deployment phases, status light, notification diffing, metric types |
| `src/api.rs` | Laravel Cloud API client |
| `src/poller.rs` | Background polling across organizations |
| `src/charts.rs` | Chart, meter and icon rendering (tiny-skia, 2×) |
| `src/menu_views.rs` | Native AppKit card views inside menu items (objc2) |
| `src/windows.rs` | Settings, logs, commands and variables windows (wry webviews) |
| `src/updater.rs` | Self-update from GitHub Releases (checksum + signature checks) |
| `src/vars.rs` | Environment variable diffing and validation |
| `src/settings.rs` | Persisted settings, login item |
| `src/system.rs` | Keychain, notifications |
| `scripts/bundle.sh` | Build, sign, notarize, zip `LC Menubar.app` |
| `packaging/` | `Info.plist`, Homebrew cask template |

## Rules

- Every action that changes something must ask for confirmation first: writes to Laravel Cloud (deploy, switch branch, start/stop, purge, env var save, commands), installing an update, and removing an organization (deletes its token). Use `menu_views::confirm`, with the destructive button first.
- API tokens only go in the Keychain, never in settings files or logs. `LARAVEL_CLOUD_TOKEN` in the env adds an extra organization for development.
- Keep `--demo` working for any new UI; extend `demo_snapshot` in `src/main.rs` when adding data.
- **Every fix and feature gets a `CHANGELOG.md` entry** under `Unreleased` (`### Added`, `### Changed`, `### Fixed`), in the same commit. Write it for users: one plain sentence, what changed, no internals. The changelog is embedded in the app (Settings › What's new, `src/changelog.rs`) and becomes the GitHub release notes. CI fails when `src/`, `Cargo.toml` or `packaging/` change without it; only purely internal changes (refactors, tests, CI) may use `[skip changelog]` in the commit message. A test fails if the version in `Cargo.toml` has no changelog section.
- Don't commit `target/` or `dist/`.

## Debug and screenshots

- `LCM_POPUP=menu|0|1|…|settings|logs|commands|vars|update` opens the menu, an environment's submenu or a window by itself; `LCM_TAB=<tab>` (e.g. `news`) picks the Settings tab.
- `LCM_APPEARANCE=dark|light` forces appearance; `LCM_TEST_PERIOD=24h,1h` switches the chart range while open; `LCM_TIMING=1` prints render timings.
- `--preview-cards <dir>` renders the menu cards to PNGs; `--app-icon <dir>` writes the iconset.
- `LCM_UPDATE_REPO=owner/name` points the updater at another repo.
- README screenshots live in `docs/screenshots/`. To retake one, run `LCM_POPUP=<x> LCM_APPEARANCE=dark lc-menubar --demo` and capture only the app's window (`screencapture -o -l <windowid>`), never the full screen.

## Release

1. Move `Unreleased` notes in `CHANGELOG.md` under a new version, bump `version` in `Cargo.toml`.
2. Commit, then `git tag vX.Y.Z && git push origin vX.Y.Z`.
3. The Release workflow builds a universal app, signs and notarizes it when secrets are set (`MACOS_CERTIFICATE_P12`, `MACOS_CERTIFICATE_PASSWORD`, `MACOS_SIGN_IDENTITY`, `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD`), and publishes the zip, its `.sha256` and the changelog section.
4. Update the Homebrew tap: copy `packaging/lc-menubar.rb` to `Casks/lc-menubar.rb` in [mstaack/homebrew-tap](https://github.com/mstaack/homebrew-tap) with the new `version` and the `sha256` from the release's `.sha256` file, and push.

Releases are not signed yet (no signing secrets): the cask clears the quarantine flag after install, and the updater checks checksums only. Check cask changes with `brew style` and `brew audit --cask --online mstaack/tap/lc-menubar`; don't use `uninstall delete:` (it runs with sudo).

Once signed, the updater only accepts updates signed by the same Team ID.
