# Changelog

All notable changes to LC Menubar are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/). Each release's notes on GitHub, and the
What's new page in Settings, are taken from this file.

## [Unreleased]

### Changed

- CPU and memory charts use a fixed 0–100% scale, like Laravel Cloud's dashboard, so small changes no longer look like big swings.

### Fixed

- An environment could keep showing an old cancelled deployment (such as "Deployments are temporarily disabled while Laravel Cloud performs security maintenance") instead of its latest deploy.
- Release notes in the update dialog show as plain text instead of Markdown.

## [0.1.1] - 2026-11-11

### Added

- Show or hide whole projects in Settings › Projects. A hidden project's environments stay hidden, including ones added later.

### Changed

- Settings and the other windows open much faster, and appear only once they are ready instead of showing an empty frame first.

### Fixed

- Parts of the menu could come up blank after environments appeared or were hidden.

## [0.1.0] - 2026-11-11

First public release.

### Added

- Status dot in the menu bar: red when a deploy failed, amber while deploying, green when everything is deployed. Optionally shows this period's spend next to it.
- Every environment across your organizations, with branch, age, CPU and replica count.
- A card per environment: the latest deployment (and why it failed), CPU, memory and request charts, replicas and web workers. Switch between 1h, 6h, 24h and 7d without closing the menu.
- Actions: deploy, live logs, run commands, edit environment variables, open the site, copy the commit hash, start or stop the environment, purge the edge cache. Every change asks first.
- Spend per organization: this billing period, bandwidth and the alert threshold.
- Several organizations, each with its own API token kept in the Keychain, and a switch in the menu.
- Notifications when deployments start, finish, fail or are cancelled.
- Settings: open at login, refresh interval, notifications, menu and metrics options, hidden environments, saved commands, and What's new.
- Updates itself from GitHub releases after checking the download's checksum.
- About LC Menubar in the menu, with the version and credits.
- Install with Homebrew: `brew install --cask mstaack/tap/lc-menubar`.

[Unreleased]: https://github.com/mstaack/lc-menubar/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/mstaack/lc-menubar/releases/tag/v0.1.1
[0.1.0]: https://github.com/mstaack/lc-menubar/releases/tag/v0.1.0
