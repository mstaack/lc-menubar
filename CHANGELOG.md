# Changelog

All notable changes to LC Menubar are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/). Each release's notes on GitHub, and the
What's new page in Settings, are taken from this file.

## [Unreleased]

## [0.1.11] - 2026-11-17

### Changed

- The bandwidth row in the menu now reads "Bandwidth used" and shows the extra cost in brackets once you go over your plan's allowance.

## [0.1.10] - 2026-11-16

### Added

- The menu shows the spend total and bandwidth usage for the current billing period again, in a compact row (Settings › Menu and metrics).

## [0.1.9] - 2026-11-16

### Changed

- The instances and domains lists are gone from the environment menu; they added more noise than help.

## [0.1.8] - 2026-11-16

### Changed

- The spend card is gone from the menu. The spend total can still be shown next to the status dot (Settings › Menu and metrics).

## [0.1.7] - 2026-11-16

### Added

- The deployment card shows the PHP and Node versions, and whether Octane or hibernation is on.
- The spend card lists what each project cost this period.
- Each environment shows its instances with size and scaling (replicas, autoscaling thresholds).
- Each environment lists its domains and warns when DNS, SSL or the origin needs attention.
- Failed jobs of managed queues appear in the environment's menu, where you can retry or delete them.

### Changed

- The spend card shows the dates of the current billing period.
- "Copy commit hash" is now "Open commit on GitHub", which opens the latest deployment's commit and its diff (projects without a GitHub repository still copy the hash).
- Settings now say where LC Menubar is made: Hamburg, Germany.

## [0.1.6] - 2026-11-11

### Fixed

- CPU and memory showed 100 times too low (13% as 0.13%): Laravel Cloud reports them as fractions, which LC Menubar now converts to percent.

## [0.1.5] - 2026-11-11

### Changed

- New versions are offered in their own window, with the changes grouped under coloured labels, instead of a plain alert.
- What's new in Settings has the same clearer look: a card per version, coloured labels for Added, Changed and Fixed, and an Installed badge.

## [0.1.4] - 2026-11-11

### Added

- Switch branch in each environment's menu: pick one of the project's recent branches, or type another, and LC Menubar switches the environment to it and deploys.

### Changed

- Removing an organization in Settings asks first, like every other action that changes something.

## [0.1.3] - 2026-11-11

### Changed

- Check for updates in Settings offers to install a new version right away, instead of pointing to the menu.
- Release notes are written as plain text, so the update dialog of every version shows them cleanly.

## [0.1.2] - 2026-11-11

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

[Unreleased]: https://github.com/mstaack/lc-menubar/compare/v0.1.11...HEAD
[0.1.11]: https://github.com/mstaack/lc-menubar/releases/tag/v0.1.11
[0.1.10]: https://github.com/mstaack/lc-menubar/releases/tag/v0.1.10
[0.1.9]: https://github.com/mstaack/lc-menubar/releases/tag/v0.1.9
[0.1.8]: https://github.com/mstaack/lc-menubar/releases/tag/v0.1.8
[0.1.7]: https://github.com/mstaack/lc-menubar/releases/tag/v0.1.7
[0.1.6]: https://github.com/mstaack/lc-menubar/releases/tag/v0.1.6
[0.1.5]: https://github.com/mstaack/lc-menubar/releases/tag/v0.1.5
[0.1.4]: https://github.com/mstaack/lc-menubar/releases/tag/v0.1.4
[0.1.3]: https://github.com/mstaack/lc-menubar/releases/tag/v0.1.3
[0.1.2]: https://github.com/mstaack/lc-menubar/releases/tag/v0.1.2
[0.1.1]: https://github.com/mstaack/lc-menubar/releases/tag/v0.1.1
[0.1.0]: https://github.com/mstaack/lc-menubar/releases/tag/v0.1.0
