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

CI (`.github/workflows/ci.yml`) runs a changelog check, fmt, clippy with `-D warnings` and tests on macOS. Run all three before finishing. After a version bump, build once before clippy so `Cargo.lock` is updated (`--locked` fails otherwise).

## Layout

| File | Purpose |
| --- | --- |
| `src/main.rs` | Event loop, tray, menu building, actions (deploy, switch branch, …), About, demo data |
| `src/model.rs` | Applications, deployment phases, status light, notification diffing, metric types |
| `src/api.rs` | Laravel Cloud API client |
| `src/poller.rs` | Background polling across organizations, skips hidden projects |
| `src/charts.rs` | Chart, meter and icon rendering (tiny-skia, 2×) |
| `src/menu_views.rs` | Native AppKit card views inside menu items, `confirm`/`ask`/`prompt` dialogs (objc2) |
| `src/windows.rs` | Settings, logs, commands, variables and update windows (wry webviews) |
| `src/changelog.rs` | Embedded `CHANGELOG.md`, What's new, parsing of release notes |
| `src/updater.rs` | Self-update from GitHub Releases (checksum + signature checks) |
| `src/vars.rs` | Environment variable diffing and validation |
| `src/settings.rs` | Persisted settings (incl. `hidden_apps`), login item |
| `src/system.rs` | Keychain, notifications |
| `scripts/bundle.sh` | Build, sign, notarize, zip `LC Menubar.app` |
| `scripts/release-notes.sh` | Changelog section → plain-text release notes |
| `packaging/` | `Info.plist`, Homebrew cask template |

## Rules

- Every action that changes something must ask for confirmation first: writes to Laravel Cloud (deploy, switch branch, start/stop, purge, env var save, commands), installing an update, and removing an organization (deletes its token). Use `menu_views::confirm`, with the destructive button first. Say exactly what will happen (e.g. the env var keys set and deleted).
- API tokens only go in the Keychain, never in settings files or logs. `LARAVEL_CLOUD_TOKEN` in the env adds an extra organization for development.
- Keep `--demo` working for any new UI; extend `demo_snapshot` in `src/main.rs` when adding data.
- **Every fix and feature gets a `CHANGELOG.md` entry** under `Unreleased` (`### Added`, `### Changed`, `### Fixed`), in the same commit. Write it for users: one plain sentence, what changed, no internals. The changelog is embedded in the app (Settings › What's new, `src/changelog.rs`) and becomes the GitHub release notes. CI fails when `src/`, `Cargo.toml` or `packaging/` change without it; only purely internal changes (refactors, tests, CI) may use `[skip changelog]` in the commit message. A test fails if the version in `Cargo.toml` has no changelog section.
- Don't commit `target/` or `dist/`.

## Laravel Cloud API notes

- JSON:API. Pagination only follows `links.next` that point at the API base.
- Deployments come newest first. Cancelled ones can have `started_at: null`; `newest` in `src/api.rs` ranks by `started_at` or `finished_at` and only treats a deployment without times as newest while it is still building. Don't go back to "first in the list".
- Metrics: `cpu_usage` and `memory_usage` are fractions (`max: 1`); `parse_metric` scales them to percent like the web UI.
- Switch branch is `PATCH /environments/{id}` with `branch`, followed by a deploy. There is no endpoint that lists branches: the choices come from `repository.default_branch`, the current branch and branches of recent deployments (`Application::branch_choices`).
- The variables endpoint with `method: "set"` updates matching keys and adds the rest.
- The `cloud` CLI (if installed) is handy for checking what the API returns; it is read-only use only, never deploy from it.

## Menu and windows

- The menu is updated in place when its structure is unchanged (titles, card views) and rebuilt otherwise; action payloads are part of the menu signature. Card views are cached per key and each cached view is used at most once; call `menu_views::forget_views()` before attaching a new menu, or cards show up blank.
- Card width is `CARD_W` (340); meta lines are truncated to `ROW_CHARS` so they don't run into the right column.
- Web windows open hidden and show on the page's `ready` IPC message (1.5 s fallback), so they never flash empty. A hidden web view is warmed up at start so the first Settings open is fast. Navigation away from the app's own pages is blocked.
- Settings tabs: General (incl. credits), Organizations, Notifications, Menu and metrics, Projects (show/hide projects and environments), Commands, What's new. Release notes in What's new and the update window share one renderer with coloured Added/Changed/Fixed tags.

## Testing in the VM

Run the app, demo and screenshots in the VMPal macOS VM ("LC Menubar macOS"), not on the host:

1. `UNIVERSAL=0 scripts/bundle.sh`, then send the zip to the VM with `vmpal_send_files` (the shared folder is not reliable).
2. In the VM: `~/lcm/install.sh` installs it, `~/lcm/run.sh <popup> [appearance] [tab] [period]` starts it with the `LCM_*` variables below, `~/lcm/wincap <dir>` captures app windows only. Keep the display awake with `caffeinate -d`.
3. Check with `vmpal_tree`/`vmpal_observe` first, screenshots only when needed.

## Debug and screenshots

- `LCM_POPUP=menu|0|1|…|settings|logs|commands|vars|update` opens the menu, an environment's submenu or a window by itself; `LCM_TAB=<tab>` (e.g. `news`) picks the Settings tab.
- `LCM_APPEARANCE=dark|light` forces appearance; `LCM_TEST_PERIOD=24h,1h` switches the chart range while open; `LCM_TIMING=1` prints render and window timings.
- `--preview-cards <dir>` renders the menu cards to PNGs; `--app-icon <dir>` writes the iconset.
- `LCM_UPDATE_REPO=owner/name` points the updater at another repo.
- README screenshots live in `docs/screenshots/`, all in dark mode. To retake one, run `LCM_POPUP=<x> LCM_APPEARANCE=dark lc-menubar --demo` and capture only the app's window (`screencapture -o -l <windowid>`), never the full screen.

## Release

1. Move `Unreleased` notes in `CHANGELOG.md` under `## [X.Y.Z] - YYYY-MM-DD`, add its release link at the bottom, bump `version` in `Cargo.toml`, build so `Cargo.lock` follows.
2. Commit as `Release X.Y.Z`, push, wait for CI to pass, then `git tag vX.Y.Z && git push origin vX.Y.Z`.
3. The Release workflow builds a universal app, signs and notarizes it when secrets are set (`MACOS_CERTIFICATE_P12`, `MACOS_CERTIFICATE_PASSWORD`, `MACOS_SIGN_IDENTITY`, `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD`), and publishes the zip, its `.sha256` and the changelog section as plain-text notes (`scripts/release-notes.sh`; the app parses that format too).
4. Update the Homebrew tap: in [mstaack/homebrew-tap](https://github.com/mstaack/homebrew-tap) set `version` and `sha256` (from the release's `.sha256` file) in `Casks/lc-menubar.rb` (based on `packaging/lc-menubar.rb`), and push.

Releases are not signed yet (no signing secrets): the cask clears the quarantine flag in `postflight_steps`, and the updater checks checksums only. Users install with `brew tap mstaack/tap && brew trust mstaack/tap && brew install --cask lc-menubar` (Homebrew requires trusting third-party taps). Check cask changes with `brew style` and `brew audit --cask --online mstaack/tap/lc-menubar`; don't use `uninstall delete:` (it runs with sudo), files go under `zap`.

Once signed, the updater only accepts updates signed by the same Team ID.
