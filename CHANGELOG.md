# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-09-30

First public release.

### Added

- Space Map: interactive sunburst with zoom, breadcrumbs, synchronized list,
  search, largest-files view, colour by folder, file type or age, and
  JSON/CSV/SVG export.
- Parallel scanner with hard-link de-duplication, symlink loop protection,
  cloud-placeholder awareness, pause/resume/cancel, and bulk directory reads
  on macOS.
- DevSweep: project and artifact detection for 20 ecosystems from TOML rules,
  risk tags, staleness from source files only, global caches and Docker usage.
- DuplicateLens: size, partial-hash and BLAKE3 passes, optional byte-by-byte
  verification, hash cache, similar images, auto-select rules, and clone or
  hard-link replacement.
- Trash Drawer with dry run, move to trash (with quarantine fallback),
  permanent delete with confirmation, revalidation, and an operation journal.
- Menu bar with keyboard shortcuts for every command.
- Headless `spacerazer-cli`.
- Release builds for macOS (universal), Windows (x86_64) and Linux
  (x86_64, aarch64; `.deb` and `.tar.gz`).

[Unreleased]: https://github.com/theArjun/spacerazer/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/theArjun/spacerazer/releases/tag/v0.1.0
