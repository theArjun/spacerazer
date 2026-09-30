# SpaceRazer Suite — Software Requirements Specification

**Version:** 1.0 (Draft) **Date:** 29 September 2026 **Status:** For review **Implementation language:** Rust (stable, edition 2024) **GUI framework:** `eframe` / `egui` (primary), with `iced` evaluated as an alternative

---

## Revision History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0 | 2026-09-29 | — | Initial draft covering Space Map, DevSweep and DuplicateLens modules |

---

## 1. Introduction

### 1.1 Purpose

This document specifies the functional and non-functional requirements for **SpaceRazer**, a cross-platform desktop application for analysing and reclaiming disk space. It is intended for the developers, testers, designers and maintainers who will build and verify the product, and serves as the baseline against which the implementation is accepted.

### 1.2 Product Scope

SpaceRazer is a single native application composed of three modules that share one scanning engine, one file index and one deletion pipeline:

1. **Space Map** — an interactive, multi-ring sunburst visualisation of storage usage (a DaisyDisk analogue for Linux, Windows and macOS).
2. **DevSweep** — a developer build-artifact and cache cleaner that detects project types and their regenerable outputs (`target/`, `node_modules/`, `.next/`, virtual environments, Xcode DerivedData, Docker data, and so on).
3. **DuplicateLens** — a duplicate file finder using a multi-pass algorithm (size → partial hash → full hash) plus perceptual similarity for images and video.

All three modules feed a shared **Trash Drawer**, a staging area where the user reviews every item before any destructive action is performed.

The goals of the product are to let a user understand where their space went within seconds of launching the app, to reclaim space safely with no accidental data loss, and to do both on very large file trees (tens of millions of entries) without UI lag.

### 1.3 Definitions, Acronyms and Abbreviations

| Term | Definition |
| --- | --- |
| Sunburst | A radial chart where each ring represents a directory depth and each arc's angle is proportional to its size |
| Node | An entry in the in-memory file tree (file, directory, symlink or special file) |
| Allocated size | Bytes actually consumed on disk (block-rounded, accounts for sparse and compressed files) |
| Apparent size | The logical length of a file as reported by metadata |
| Artifact | A regenerable file or directory produced by a build tool, package manager or cache |
| Stale project | A project whose source files have not been modified within a user-defined threshold |
| Partial hash | A hash computed over a fixed-size head and tail of a file |
| Perceptual hash (pHash/dHash) | A compact fingerprint of visual content where similar images yield hashes with small Hamming distance |
| Reflink / clone | A copy-on-write copy sharing physical blocks (APFS `clonefile`, Btrfs/XFS `FICLONE`, ReFS block cloning) |
| Trash Drawer | SpaceRazer's staging list of items pending deletion |
| OS Trash | The platform recycle facility (Recycle Bin, macOS Trash, freedesktop.org Trash) |
| Dry run | A simulated execution that reports what would happen without modifying the filesystem |
| LOD | Level of detail — rendering simplification based on on-screen size |

### 1.4 References

- ISO/IEC/IEEE 29148:2018, *Requirements engineering*
- freedesktop.org Trash Specification 1.0
- Rust crate documentation: `eframe`/`egui`, `jwalk`, `ignore`, `rayon`, `crossbeam-channel`, `blake3`, `xxhash-rust`, `memmap2`, `image`, `image_hasher`, `trash`, `sysinfo`, `notify`, `serde`, `redb`
- Web Content Accessibility Guidelines (WCAG) 2.2, for contrast and non-colour cues

### 1.5 Document Overview

Section 2 describes the product context, users and constraints. Section 3 lists functional requirements per module. Section 4 covers external interfaces. Section 5 sets non-functional requirements. Section 6 outlines the architecture and data model. Section 7 defines the deletion safety model. Sections 8–10 cover platform specifics, acceptance criteria and the release plan. Appendices contain the DevSweep detection rule table and open questions.

Requirement IDs follow the pattern `FR-<AREA>-<NN>` (functional) and `NFR-<AREA>-<NN>` (non-functional). Priority uses MoSCoW: **M** (Must), **S** (Should), **C** (Could), **W** (Won't, this release).

---

## 2. Overall Description

### 2.1 Product Perspective

SpaceRazer is a standalone, offline desktop application. It requires no network access, no account and no background daemon. It interacts with the local filesystem, the OS trash facility, and optionally with external tools (Docker CLI/Engine API, `ffmpeg`) when present.

```
┌──────────────────────────────── GUI (eframe/egui) ────────────────────────────────┐
│  Space Map view   │   DevSweep view   │   DuplicateLens view   │   Trash Drawer    │
└─────────┬─────────┴─────────┬─────────┴───────────┬────────────┴─────────┬─────────┘
          │  commands / progress events (crossbeam channels)               │
┌─────────▼───────────────────▼─────────────────────▼──────────────────────▼─────────┐
│  Scan Engine  →  File Index (arena tree)  →  Analyzers (DevSweep, Dedup)  →  Ops  │
└──────────────────────────────────────────────────────────────────────────────────┘
          │                                                      │
     Filesystem (read)                               OS Trash / delete / link (write)
```

### 2.2 Product Functions (Summary)

- Scan one or more volumes or folders in parallel and build an in-memory size tree.
- Render that tree as a zoomable, clickable sunburst with breadcrumbs and a synchronized list view.
- Detect developer projects, their artifacts and their staleness; offer bulk clean-up with dry run.
- Find exact duplicates and visually similar media; offer delete, hardlink or reflink resolution.
- Stage all destructive actions in a Trash Drawer, then execute them safely with a journal.

### 2.3 User Classes and Characteristics

| User class | Description | Primary modules |
| --- | --- | --- |
| General user | Wants to know why their disk is full and free space quickly; low tolerance for jargon | Space Map, Trash Drawer |
| Software developer | Many repositories and toolchains; comfortable with terminology; wants bulk actions | DevSweep, Space Map |
| Photographer / media hoarder | Large photo and video libraries with many near-duplicates | DuplicateLens |
| Power user / sysadmin | Scans multiple volumes, uses exclusion rules, exports reports, may use CLI mode | All |

### 2.4 Operating Environment

| Platform | Minimum version | Architectures |
| --- | --- | --- |
| Windows | Windows 10 22H2 | x86_64, aarch64 |
| macOS | macOS 13 Ventura | aarch64 (Apple Silicon), x86_64 |
| Linux | glibc 2.31+ distributions; X11 and Wayland | x86_64, aarch64 |

Rendering backend: `wgpu` (Vulkan, Metal, DX12) with automatic fallback to `glow` (OpenGL 3.3 / GLES) where `wgpu` is unavailable.

Reference hardware for performance targets: 8-core CPU, 16 GB RAM, NVMe SSD (\~3 GB/s sequential read). Minimum supported: 4-core CPU, 8 GB RAM, SATA SSD or HDD.

### 2.5 Design and Implementation Constraints

- **C-1** The application shall be written in safe Rust; `unsafe` is permitted only in isolated, reviewed modules (memory mapping, platform FFI) with documented invariants.
- **C-2** The GUI shall use `eframe`/`egui` so that the sunburst can be drawn with custom tessellated meshes on the GPU. (Rationale: immediate-mode drawing and direct access to the painter make per-frame arc rendering and hit-testing simpler than a retained widget tree. `iced` remains a documented alternative; see Appendix C.)
- **C-3** No component shall require network access. No telemetry shall be collected.
- **C-4** The application shall never require root/administrator privileges to launch. Elevated scans shall be opt-in per session.
- **C-5** Distribution: single binary per platform plus platform installers (MSI, signed/notarized `.dmg`, AppImage, `.deb`, `.rpm`, Flatpak).
- **C-6** License of all dependencies must be compatible with the product license (to be decided; see Appendix D).

### 2.6 Assumptions and Dependencies

- The OS exposes a trash facility usable by the `trash` crate on each target platform; where it doesn't (e.g. some network or removable volumes), the app falls back to its own quarantine folder (see §7).
- Docker analysis depends on a reachable Docker Engine API or `docker` CLI. If absent, Docker features are hidden.
- Video perceptual hashing depends on an `ffmpeg` binary found on `PATH` or configured by the user. If absent, video similarity is disabled and exact-hash dedup still works.
- macOS scans of protected locations require the user to grant Full Disk Access; the app shall detect and explain this.

---

## 3. Functional Requirements

### 3.1 Scan Engine (shared)

| ID | Requirement | Priority |
| --- | --- | --- |
| FR-SCAN-01 | The system shall list mounted volumes with total, used and free space (via `sysinfo`) on the home screen. | M |
| FR-SCAN-02 | The user shall be able to scan a whole volume, one or more chosen folders, or a folder dropped onto the window. | M |
| FR-SCAN-03 | The scanner shall traverse directories in parallel using a work-stealing walker (`jwalk` for raw speed; `ignore` when gitignore-aware mode is enabled). | M |
| FR-SCAN-04 | The scanner shall stream partial results to the UI so the sunburst appears and grows during the scan, not only at the end. | M |
| FR-SCAN-05 | The scanner shall record for each node: name, parent, kind, apparent size, allocated size, modified time, and (where available) device ID and inode/file index. | M |
| FR-SCAN-06 | Hardlinked files shall be counted once in aggregate totals, identified by (device, inode) on Unix and (volume serial, file index) on Windows. | M |
| FR-SCAN-07 | Symlinks, junctions and reparse points shall not be followed by default. The user may enable following, with cycle detection. | M |
| FR-SCAN-08 | The scanner shall not cross filesystem boundaries unless the user enables it. | M |
| FR-SCAN-09 | Unreadable entries (permission denied, I/O error) shall be recorded with the error, counted, and listed in a "Scan issues" panel; the scan shall continue. | M |
| FR-SCAN-10 | The user shall be able to pause, resume and cancel a scan. Cancel shall stop all worker threads within 500 ms. | M |
| FR-SCAN-11 | The user shall be able to define exclusion rules (paths and glob patterns). Built-in defaults exclude virtual filesystems (`/proc`, `/sys`, `/dev`, `/run`) and cloud-placeholder contents that would trigger downloads. | M |
| FR-SCAN-12 | The user shall choose between "allocated size" (default) and "apparent size" for display. | S |
| FR-SCAN-13 | The system shall optionally cache scan results to disk (`redb`) and offer "rescan changed folders only" on next launch, using directory mtimes to skip unchanged subtrees. | S |
| FR-SCAN-14 | The system shall optionally watch scanned roots for changes (`notify`) and update the tree incrementally while the app is open. | C |
| FR-SCAN-15 | On Windows NTFS volumes with administrator rights, the system may read the Master File Table directly for a faster scan. | C |
| FR-SCAN-16 | Cloud placeholder files (OneDrive Files On-Demand, iCloud "optimized" files, Dropbox online-only) shall be detected and shown with their allocated (local) size and a cloud badge; the scanner shall never trigger their hydration. | M |

### 3.2 Space Map (Sunburst Visualisation)

| ID | Requirement | Priority |
| --- | --- | --- |
| FR-MAP-01 | The system shall render the current directory as the chart centre with its descendants as concentric rings, arc angle proportional to size. | M |
| FR-MAP-02 | The chart shall display a configurable number of rings (default 5, range 2–10). | M |
| FR-MAP-03 | Arcs smaller than a minimum angle (default 0.5°) shall be aggregated into a single "smaller items" arc per parent to preserve frame rate and legibility. | M |
| FR-MAP-04 | Hovering an arc shall highlight it and show a tooltip with name, size, percentage of parent, percentage of volume, item count and last-modified date. | M |
| FR-MAP-05 | Clicking a directory arc shall zoom into it with an animated transition (default 250 ms, respects "reduce motion" setting). | M |
| FR-MAP-06 | Clicking the centre, pressing Backspace, or clicking a breadcrumb segment shall zoom out to that ancestor. | M |
| FR-MAP-07 | A breadcrumb bar shall always show the path from the scan root to the current centre, with each segment clickable. | M |
| FR-MAP-08 | A synchronized list panel shall show the current directory's children sorted by size, with hover and selection mirrored between list and chart. | M |
| FR-MAP-09 | Arc colours shall be assigned by top-level branch (hue) and depth (lightness). Optional colour modes: by file type category, by age. | S |
| FR-MAP-10 | Colour shall never be the only carrier of meaning; the tooltip and list shall convey the same information. A high-contrast palette shall be available. | M |
| FR-MAP-11 | Right-click (or context key) on any arc or list row shall offer: Reveal in file manager, Open, Copy path, Add to Trash Drawer, Exclude from scan, Show in DevSweep (if applicable). | M |
| FR-MAP-12 | Items may be dragged from the chart or list onto the Trash Drawer. | S |
| FR-MAP-13 | Items staged in the Trash Drawer shall appear hatched or dimmed in the chart, and a "space to reclaim" figure shall update live. | M |
| FR-MAP-14 | A search box shall filter/highlight nodes whose names match a substring or glob, across the whole tree. | S |
| FR-MAP-15 | A "Top 100 largest files" view shall be available for the scanned tree. | S |
| FR-MAP-16 | The user shall be able to export the tree summary as CSV or JSON, and the current chart as PNG or SVG. | C |
| FR-MAP-17 | Keyboard navigation: arrow keys move selection between sibling arcs and rings; Enter zooms; Backspace zooms out; Delete stages the selection. | M |

### 3.3 Trash Drawer (shared)

| ID | Requirement | Priority |
| --- | --- | --- |
| FR-TRASH-01 | The Trash Drawer shall be a persistent side panel listing every staged item with path, size, source module (Map / DevSweep / DuplicateLens) and reason. | M |
| FR-TRASH-02 | The drawer shall show total reclaimable space, accounting for hardlinks (an item whose other links survive frees nothing) and for nested selections (a child of a staged folder is not double-counted). | M |
| FR-TRASH-03 | The user shall be able to remove individual items or clear the drawer without any filesystem effect. | M |
| FR-TRASH-04 | The drawer shall offer three actions: **Move to OS Trash** (default), **Delete permanently**, and **Dry run**. | M |
| FR-TRASH-05 | Dry run shall produce a report of every operation that would be performed, with expected space reclaimed, and shall make no filesystem changes. | M |
| FR-TRASH-06 | Permanent deletion shall require an explicit confirmation dialog stating the item count and total size, and requiring a deliberate action (typed confirmation or hold-to-confirm) when total exceeds a configurable threshold (default 10 GB or 1,000 items). | M |
| FR-TRASH-07 | Before executing, each item shall be revalidated (still exists, same kind, same size and mtime as when staged). Changed items shall be skipped and reported. | M |
| FR-TRASH-08 | Execution shall run in the background with progress, be cancellable between items, and produce a final report (succeeded, skipped, failed, bytes freed). | M |
| FR-TRASH-09 | Every executed operation shall be appended to an operation journal (see §7.4) viewable in the app. | M |
| FR-TRASH-10 | The drawer contents shall persist across app restarts until executed or cleared. | S |
| FR-TRASH-11 | For items moved to OS Trash, the report shall offer "Restore" where the platform supports it. | C |

### 3.4 DevSweep (Developer Build & Cache Cleaner)

| ID | Requirement | Priority |
| --- | --- | --- |
| FR-DEV-01 | The system shall detect projects by marker files (e.g. `Cargo.toml`, `package.json`, `pyproject.toml`, `pom.xml`, `build.gradle(.kts)`, `*.xcodeproj`, `go.mod`, `CMakeLists.txt`, `pubspec.yaml`) using the rule table in Appendix A. | M |
| FR-DEV-02 | An artifact directory shall be flagged only when its matching marker is present in the expected location (e.g. `target/` is flagged only next to a `Cargo.toml` or `pom.xml`). Generic names such as `build/` or `dist/` shall never be flagged without a marker. | M |
| FR-DEV-03 | The system shall detect global toolchain caches (e.g. `~/.cargo/registry`, `~/.npm`, pnpm store, `~/.gradle/caches`, `~/go/pkg/mod`, pip cache, Xcode DerivedData, iOS DeviceSupport, CocoaPods cache) with per-platform default paths. | M |
| FR-DEV-04 | Docker usage shall be reported via the Docker Engine API or `docker system df` (images, containers, volumes, build cache). Docker data shall only be cleaned through Docker's own prune commands, never by deleting files in Docker's data directory. | S |
| FR-DEV-05 | For each project the system shall show: name, path, type(s), artifact size, source size, last source modification date, and (if a Git repo) last commit date. | M |
| FR-DEV-06 | "Last activity" shall be computed from source files only, excluding artifact directories and VCS metadata, so that a recent build does not make an abandoned project look active. | M |
| FR-DEV-07 | The user shall be able to filter projects by type, by minimum artifact size, and by inactivity (e.g. "untouched for more than 30 / 90 / 180 days / custom"). | M |
| FR-DEV-08 | "Select all stale" shall select artifacts of all projects past the inactivity threshold, excluding pinned projects. | M |
| FR-DEV-09 | The user shall be able to pin a project (never suggest cleaning it) and to add custom detection rules (marker glob + artifact path) via settings. | S |
| FR-DEV-10 | Each artifact shall show a "regenerate with" hint (e.g. `cargo build`, `npm install`) and a risk tag: **Safe** (fully regenerable), **Caution** (regenerable but slow or network-dependent, e.g. `node_modules` of a project without a lockfile), **Review** (may contain user state, e.g. `.venv` with manually installed packages). | M |
| FR-DEV-11 | Where a tool provides an official clean command (e.g. `cargo clean`, `docker builder prune`), the user may choose "use tool's clean command" instead of direct deletion; the command and its output shall be shown. | C |
| FR-DEV-12 | "Clean selected" shall send items to the Trash Drawer (default) or, if the user enables "express mode", execute immediately after a dry-run summary and confirmation. | M |
| FR-DEV-13 | The system shall never flag an artifact that contains a detected project marker for a *different* project (nested repositories are treated as their own projects). | M |
| FR-DEV-14 | Artifact directories shall be excluded from Git status concerns: if an artifact path is tracked by Git (not ignored), it shall be tagged **Review** rather than **Safe**. | S |

### 3.5 DuplicateLens (Duplicate and Similarity Finder)

#### 3.5.1 Exact duplicates

| ID | Requirement | Priority |
| --- | --- | --- |
| FR-DUP-01 | The user shall choose one or more roots, a minimum file size (default 1 MB, minimum 1 byte), and include/exclude patterns. | M |
| FR-DUP-02 | Pass 1 shall group candidate files by exact size; unique sizes are discarded. | M |
| FR-DUP-03 | Files that are already hardlinks of each other (same device + inode/file index) shall be recognised as one physical file and not reported as duplicates. | M |
| FR-DUP-04 | Pass 2 shall compute a fast partial hash (xxh3-128) over the first and last 16 KiB (configurable) of each remaining candidate and regroup. | M |
| FR-DUP-05 | Pass 3 shall compute a full BLAKE3 hash of remaining candidates. Files larger than a threshold (default 64 MiB) shall be hashed via memory mapping (`memmap2`) and BLAKE3's parallel hashing; smaller files via buffered reads. | M |
| FR-DUP-06 | An optional "paranoid" mode shall perform byte-by-byte comparison within each final group before any destructive action. | S |
| FR-DUP-07 | Hashing shall be parallelised with `rayon`, with I/O concurrency tuned per device type (high for SSD, low for HDD to avoid seek thrashing). Device type detection shall be automatic with manual override. | M |
| FR-DUP-08 | Hashes shall be cached keyed by (path, size, mtime, inode) so rescans only rehash changed files. | S |
| FR-DUP-09 | Progress shall show per-pass counts, bytes hashed, throughput and ETA. | M |

#### 3.5.2 Visual similarity

| ID | Requirement | Priority |
| --- | --- | --- |
| FR-DUP-10 | For images (JPEG, PNG, WebP, GIF, BMP, TIFF; HEIC/RAW where decoders are available), the system shall compute perceptual hashes (dHash and pHash via `image_hasher`) on a downscaled decode. | S |
| FR-DUP-11 | Similar images shall be grouped by Hamming distance below a user-adjustable threshold (a "similarity" slider), using a BK-tree or equivalent index to avoid O(n²) comparisons. | S |
| FR-DUP-12 | Image decoding shall respect EXIF orientation so rotated copies are matched. | S |
| FR-DUP-13 | For videos, when `ffmpeg` is available, the system shall sample N frames at fixed relative positions (default 10), hash each, and compare sequences; duration difference beyond a tolerance shall exclude a pair. | C |
| FR-DUP-14 | Perceptual matches shall always be presented as "similar", visually distinct from "identical", and shall never be auto-selected for deletion. | M |

#### 3.5.3 Review and resolution

| ID | Requirement | Priority |
| --- | --- | --- |
| FR-DUP-15 | Results shall be shown as groups sorted by wasted space (size × (copies − 1)), with group count and total reclaimable space. | M |
| FR-DUP-16 | Selecting a group shall show a side-by-side comparison: thumbnails/previews for media, text preview for text, and metadata (path, size, dates, dimensions, EXIF camera/date where present). | M |
| FR-DUP-17 | The system shall offer auto-select rules that keep exactly one file per group: keep oldest, keep newest, keep shortest path, keep file inside a preferred folder, keep highest resolution (similar mode). The UI shall prevent selecting every member of a group. | M |
| FR-DUP-18 | Resolution actions: **Stage for deletion** (to Trash Drawer), **Replace with hardlink**, **Replace with reflink/clone** (where the filesystem supports it). | M |
| FR-DUP-19 | Hardlink replacement shall only be offered when all files are on the same volume, and shall warn that edits to one path affect all linked paths. Reflinks shall be preferred when available because they keep copies independent. | M |
| FR-DUP-20 | Link replacement shall be atomic per file: create link at a temporary name in the same directory, verify content hash, then rename over the original. On any failure the original remains untouched. | M |
| FR-DUP-21 | Results shall be exportable as CSV/JSON. | C |

### 3.6 Settings, Reports and Miscellany

| ID | Requirement | Priority |
| --- | --- | --- |
| FR-SET-01 | Settings shall include: exclusions, protected paths, size mode, ring count, colour scheme, theme (light/dark/system), reduce motion, thread limits, hash cache location and size, delete confirmation thresholds, DevSweep rules and thresholds, `ffmpeg` path. | M |
| FR-SET-02 | Settings shall be stored as human-readable TOML in the platform config directory (via `directories`). | M |
| FR-SET-03 | A built-in protected-path list (OS directories, the user's home root itself, the app's own install directory, mounted volume roots) shall prevent staging those paths; the list can be extended but built-in entries cannot be removed. | M |
| FR-SET-04 | A headless CLI (\`spacerazer scan | dev |
| FR-SET-05 | The app shall provide an in-app log viewer and "Copy diagnostics" for bug reports; logs shall not contain file contents. | S |

---

## 4. External Interface Requirements

### 4.1 User Interface

The main window uses a three-zone layout:

```
┌──────────────────────────────────────────────────────────────────────┐
│ [Space Map] [DevSweep] [DuplicateLens]        ⌕ search     ⚙        │  ← tab bar
├──────────────────────────────────────────────────────────────────────┤
│ / ▸ home ▸ alex ▸ projects                                           │  ← breadcrumbs
├─────────────────────────────────────────┬────────────────────────────┤
│                                         │ Name              Size  %  │
│            (sunburst canvas)            │ ▸ node_modules   12.4G 31  │
│                                         │ ▸ target          9.1G 23  │
│                                         │ ...                        │
├─────────────────────────────────────────┴────────────────────────────┤
│ Trash Drawer: 14 items · 27.3 GB reclaimable   [Dry run] [Move to Trash] │
└──────────────────────────────────────────────────────────────────────┘
```

- **UI-1** Minimum window size 900 × 600 logical pixels; layout adapts from 900 px to 4K; HiDPI scaling honoured.
- **UI-2** Light, dark and system-following themes; high-contrast option.
- **UI-3** All actions reachable by keyboard; visible focus indicator; shortcuts listed in a help overlay (`?`).
- **UI-4** Screen-reader support through egui's AccessKit integration for all standard widgets; the sunburst exposes its selection and the synchronized list serves as the accessible equivalent.
- **UI-5** Long operations never block the UI thread; each shows progress and a cancel control.
- **UI-6** Destructive buttons use a distinct style and are never the default focused control in a dialog.
- **UI-7** Sizes display in binary (GiB) or decimal (GB) units per setting, default matching the host OS convention.

### 4.2 Hardware Interfaces

No direct hardware interfaces. The app detects storage device type (SSD/HDD/network) to tune I/O concurrency.

### 4.3 Software Interfaces

| Interface | Purpose | Notes |
| --- | --- | --- |
| Filesystem APIs (std, `libc`, `windows` crate) | Metadata, reading, deletion, linking | Platform-specific extensions for inode/file index, allocated size, reflink |
| OS Trash | Recoverable deletion | `trash` crate; fallback quarantine folder |
| File manager | Reveal in Finder/Explorer/file manager | `opener` crate / platform commands |
| Docker Engine API or CLI | Docker disk usage and prune | Optional; feature-gated |
| `ffmpeg` | Video frame extraction | Optional external binary; invoked as a subprocess, never linked |
| Git | Last commit date | `gix` (pure Rust) read-only |

### 4.4 Communications Interfaces

None. The application makes no network connections. (Update checks, if added later, must be opt-in.)

---

## 5. Non-Functional Requirements

### 5.1 Performance

Targets are measured on the reference hardware (§2.4) with a warm OS metadata cache unless stated otherwise.

| ID | Requirement |
| --- | --- |
| NFR-PERF-01 | Metadata scan of 1,000,000 entries on NVMe shall complete in ≤ 10 s (cold cache ≤ 30 s). |
| NFR-PERF-02 | The first partial sunburst shall render within 1 s of starting a scan. |
| NFR-PERF-03 | The UI shall sustain ≥ 60 fps during zoom/hover on a tree of 10,000,000 nodes (enabled by LOD aggregation, FR-MAP-03). Frame time budget for chart tessellation: ≤ 4 ms. |
| NFR-PERF-04 | Hover hit-testing shall resolve in ≤ 1 ms (angular binary search per ring, no per-arc linear scan). |
| NFR-PERF-05 | Memory usage shall not exceed \~100 bytes per node on average plus an interned name store (target ≤ 1.5 GB for 10 M nodes). |
| NFR-PERF-06 | Full hashing shall reach ≥ 80% of the storage device's sequential read throughput for files ≥ 64 MiB. |
| NFR-PERF-07 | Duplicate detection on 1 TB / 500,000 files with a typical duplicate ratio shall complete in minutes, bounded by the bytes that survive passes 1–2. (Note: full hashing is I/O-bound; at \~3 GB/s, every terabyte actually hashed costs about 6 minutes. "Seconds" is achievable only when size and partial-hash passes eliminate most candidates, or on a warm hash cache.) |
| NFR-PERF-08 | Rescan with a warm scan cache (FR-SCAN-13) on an unchanged tree of 1,000,000 entries shall complete in ≤ 3 s. |
| NFR-PERF-09 | Cancel of any background job shall take effect within 500 ms. |

### 5.2 Safety (Data Loss Prevention)

Safety requirements take precedence over performance and convenience requirements.

| ID | Requirement |
| --- | --- |
| NFR-SAFE-01 | No filesystem write, move, link or delete shall occur outside the Trash Drawer / DuplicateLens resolution pipeline defined in §7. |
| NFR-SAFE-02 | The default deletion method shall be recoverable (OS Trash or quarantine). |
| NFR-SAFE-03 | Deletion shall never follow symlinks or junctions: deleting a link removes the link only. |
| NFR-SAFE-04 | Protected paths (FR-SET-03) shall be rejected at staging time and again at execution time. |
| NFR-SAFE-05 | Every destructive operation shall be revalidated immediately before execution (FR-TRASH-07). |
| NFR-SAFE-06 | Any single operation failure shall not abort unrelated operations, and shall never leave a file partially deleted or replaced. |
| NFR-SAFE-07 | The deletion executor shall have ≥ 90% line coverage and property-based tests (via `proptest`) covering nested selections, links, and concurrent modification. |

### 5.3 Reliability

- **NFR-REL-01** The app shall not crash on malformed filenames (non-UTF-8 on Unix, unpaired surrogates on Windows); all paths are stored as `OsString`/`PathBuf` and displayed lossily.
- **NFR-REL-02** The app shall handle paths longer than 260 characters on Windows (`\\?\` prefix).
- **NFR-REL-03** A panic in any worker thread shall be caught, reported in the UI, and shall not corrupt the index or journal.
- **NFR-REL-04** Scan cache and hash cache corruption shall be detected (checksums/versioning) and the cache discarded rather than trusted.

### 5.4 Usability

- **NFR-USE-01** A first-time user shall be able to scan their home folder and identify their largest folder within 60 s without documentation.
- **NFR-USE-02** Every risk tag and auto-select rule shall have an inline explanation (tooltip or info icon).
- **NFR-USE-03** All animations respect the OS "reduce motion" preference.

### 5.5 Security and Privacy

- **NFR-SEC-01** No telemetry, analytics or network calls.
- **NFR-SEC-02** File contents are read only for hashing and previews; they are never persisted except as hashes and thumbnails in the local cache, which the user can clear.
- **NFR-SEC-03** Elevated scanning (admin/root) shall be run as a separate helper process with the minimum scope needed; the GUI process itself shall not run elevated.
- **NFR-SEC-04** External commands (`ffmpeg`, `docker`, tool clean commands) shall be invoked with argument vectors, never via a shell string, to prevent injection through file names.

### 5.6 Portability

- **NFR-PORT-01** One codebase; platform differences isolated behind a `platform` module with per-OS implementations and a shared trait.
- **NFR-PORT-02** CI shall build and test on Windows, macOS and Linux for every change.

### 5.7 Maintainability

- **NFR-MAIN-01** Code organised as a Cargo workspace (§6.1); core crates contain no GUI dependency and are fully unit-testable.
- **NFR-MAIN-02** `cargo clippy -D warnings` and `cargo fmt --check` gate CI; `cargo deny` checks licences and advisories.
- **NFR-MAIN-03** DevSweep rules are data (TOML), not code, so new ecosystems can be added without recompiling logic.

---

## 6. System Architecture

### 6.1 Workspace Layout

| Crate | Responsibility | Key dependencies |
| --- | --- | --- |
| `sr-core` | Node arena, size aggregation, path interning, shared types | — |
| `sr-scan` | Parallel walker, exclusions, platform metadata, scan cache | `jwalk`, `ignore`, `rayon`, `redb`, `notify` |
| `sr-devsweep` | Project detection, artifact rules, staleness, Docker integration | `serde`, `toml`, `gix` |
| `sr-dedup` | Size grouping, partial/full hashing, perceptual hashing, BK-tree | `xxhash-rust`, `blake3`, `memmap2`, `image`, `image_hasher` |
| `sr-ops` | Trash Drawer model, executor, revalidation, journal, link replacement | `trash`, platform FFI |
| `sr-platform` | OS-specific: allocated size, file IDs, reflink, device type, elevation helper | `libc`, `windows`, `core-foundation` |
| `sr-gui` | eframe app, views, sunburst renderer, state management | `eframe`, `egui`, `egui_extras` |
| `sr-cli` | Headless command-line front end | `clap` |

### 6.2 Concurrency Model

- The **UI thread** runs the egui event loop only. It never performs filesystem I/O.
- **Background jobs** (scan, DevSweep analysis, dedup, execution) run on a `rayon` thread pool (CPU-bound work) plus a bounded I/O pool for hashing, sized per device.
- Jobs communicate with the UI over `crossbeam-channel` message queues carrying progress events and batched tree deltas. The UI drains the queue each frame (bounded work per frame) and calls `ctx.request_repaint()` when new data arrives.
- Each job holds a shared `CancellationToken` (an `AtomicBool`) checked between directory entries and between hash chunks.
- `tokio` is **not** required for the core; filesystem work is blocking by nature and `rayon` fits better. `tokio` may be used only if the Docker API integration needs an async HTTP client.

### 6.3 Data Model

The file tree is stored as a flat arena for cache efficiency and to avoid millions of heap allocations:

```rust
pub struct NodeId(u32);

pub struct Node {
    pub parent: Option<NodeId>,
    pub first_child: Option<NodeId>,
    pub next_sibling: Option<NodeId>,
    pub name: NameId,          // index into an interned string arena (OsStr bytes)
    pub kind: NodeKind,        // File, Dir, Symlink, Other
    pub flags: NodeFlags,      // bitflags: hardlinked, cloud_placeholder, error, staged, excluded
    pub apparent: u64,         // own size (files) or aggregate (dirs)
    pub allocated: u64,
    pub items: u32,            // descendant count (dirs)
    pub mtime: i64,            // seconds since epoch
}

pub struct Tree {
    nodes: Vec<Node>,
    names: NameArena,
    file_ids: HashMap<(u64, u64), NodeId>, // (device, inode) for hardlink dedup
}
```

Children are sorted by size once aggregation completes (and re-sorted incrementally during streaming), which makes sunburst layout a simple cumulative-angle pass and enables binary-search hit-testing.

### 6.4 Sunburst Rendering

1. **Layout:** for the current centre node, walk descendants breadth-first up to *R* rings, assigning each child an angular span `[start, end)` proportional to its size within its parent's span. Children below the minimum angle are merged into one "smaller items" span (FR-MAP-03).
2. **Tessellation:** each arc becomes an annular sector mesh whose segment count scales with its angle and radius (LOD), emitted into a single `egui::Mesh` per frame. Layouts are cached and rebuilt only when the centre, ring count, size mode or tree data changes.
3. **Animation:** zoom transitions interpolate each arc's start/end angles and ring radius between old and new layouts with an ease-in-out curve.
4. **Hit-testing:** convert pointer position to polar (r, θ) → ring index from r → binary search the ring's sorted spans by θ.
5. **Labels:** drawn only for arcs whose arc length exceeds a text-width threshold; truncated with ellipsis.

### 6.5 Duplicate Pipeline

```
files ──► group by size ──► drop singletons & hardlink twins
      ──► xxh3(head 16K ‖ tail 16K) ──► regroup, drop singletons
      ──► BLAKE3 full (mmap if ≥64 MiB, buffered otherwise) ──► regroup
      ──► [optional] byte-compare ──► duplicate groups

images ──► decode (downscaled, EXIF-rotated) ──► dHash + pHash ──► BK-tree ──► similarity groups
videos ──► ffmpeg sample N frames ──► per-frame hashes ──► sequence distance ──► similarity groups
```

Memory mapping is used only for reading, with the file opened read-only; files that change size during hashing are dropped from the group and reported (mapped files can raise SIGBUS if truncated, so mapping is wrapped with a size check and preferring buffered reads on network filesystems).

---

## 7. Deletion Safety Model

### 7.1 Principles

1. Nothing is destroyed without first appearing in a reviewable list.
2. Recoverable deletion is the default.
3. The state at execution time, not at scan time, is what matters.
4. A partial failure never leaves a half-done item.

### 7.2 Execution Sequence per Item

1. Check against the protected-path list (reject if matched).
2. `symlink_metadata` the path (never `metadata`, which follows links). Confirm the kind, size and mtime match the staged snapshot; for directories, confirm the entry count has not grown beyond a tolerance.
3. Perform the operation (OS trash, quarantine move, or permanent delete). Directory permanent deletion walks bottom-up without following links.
4. Record the result in the journal, including bytes freed as measured, not as estimated.

### 7.3 Quarantine Fallback

Where the OS trash is unavailable for a volume, items are moved to a `.spacerazer-quarantine/<timestamp>/` folder at the root of the same volume (so the move is a rename, not a copy), and the user is told that space is freed only after emptying the quarantine from within the app.

### 7.4 Operation Journal

An append-only JSON Lines file in the app data directory, one record per operation: timestamp, operation, original path, destination (if moved), size, result, error. The journal is viewable in-app and supports "Restore from quarantine" for items still present.

---

## 8. Platform-Specific Requirements

| Area | Windows | macOS | Linux |
| --- | --- | --- | --- |
| File identity | Volume serial + file index (`GetFileInformationByHandle`) | `st_dev` + `st_ino` | `st_dev` + `st_ino` |
| Allocated size | `GetCompressedFileSizeW` | `st_blocks × 512` | `st_blocks × 512` |
| Reflink | ReFS block clone (Dev Drive) | `clonefile` (APFS) | `FICLONE` ioctl (Btrfs, XFS, bcachefs) |
| Trash | Recycle Bin via Shell API | `NSFileManager trashItem` | freedesktop.org Trash |
| Special cases | Junctions/reparse points; OneDrive placeholders; long paths; locked files | Full Disk Access prompt; SIP-protected paths; APFS clones share space (report as "may be shared"); iCloud optimised files | `/proc`, `/sys`, bind mounts, Flatpak sandbox portal for file access |
| Elevated scan | UAC-elevated helper process | Privileged helper via authorization prompt | `pkexec` helper |

**Note on APFS clones and Btrfs reflinks:** blocks shared between clones cannot be detected cheaply from standard metadata. Deleting one clone may free little or no space. The app shall label reclaim estimates on these filesystems as "up to" and report measured free-space change after execution.

---

## 9. Acceptance Criteria (Selected)

| ID | Criterion | Verifies |
| --- | --- | --- |
| AC-01 | Scanning a generated fixture of 1 M files (mixed depths) meets NFR-PERF-01 on reference hardware in CI benchmark runs. | FR-SCAN-03, NFR-PERF-01 |
| AC-02 | A fixture with hardlinks reports the physical size once; DuplicateLens does not list hardlink twins. | FR-SCAN-06, FR-DUP-03 |
| AC-03 | A symlink loop does not hang the scan; following disabled by default. | FR-SCAN-07 |
| AC-04 | Staging a symlink to a large directory and deleting it removes only the link; target intact. | NFR-SAFE-03 |
| AC-05 | Modifying a staged file between staging and execution causes it to be skipped and reported. | FR-TRASH-07 |
| AC-06 | A `build/` directory without any marker is never flagged by DevSweep; `target/` next to `Cargo.toml` is flagged as Safe. | FR-DEV-02 |
| AC-07 | A project whose `target/` was modified yesterday but whose sources were last modified 200 days ago is classified as stale at the 180-day threshold. | FR-DEV-06 |
| AC-08 | Two files identical except for one byte in the middle are grouped after pass 2 but separated by pass 3. | FR-DUP-04/05 |
| AC-09 | Auto-select can never mark all members of a group. | FR-DUP-17 |
| AC-10 | Killing the process mid-link-replacement leaves every original file readable with correct content. | FR-DUP-20 |
| AC-11 | Sunburst interaction on a 10 M-node synthetic tree holds ≥ 60 fps (p95 frame time ≤ 16.7 ms). | NFR-PERF-03 |
| AC-12 | The whole Space Map workflow (scan, navigate, stage, execute) is completable with keyboard only. | FR-MAP-17, UI-3 |

---

## 10. Release Plan

| Milestone | Scope | Exit criteria |
| --- | --- | --- |
| **M1 — Core scanner** | `sr-core`, `sr-scan`, CLI scan with JSON output | AC-01 to AC-03 pass on all three OSes |
| **M2 — Space Map MVP** | Sunburst, breadcrumbs, list sync, tooltips, context menu | AC-11, AC-12 |
| **M3 — Trash Drawer** | Staging, dry run, OS trash, permanent delete, journal | AC-04, AC-05; safety test suite green |
| **M4 — DevSweep** | Rule engine, Appendix A rules, staleness, bulk clean | AC-06, AC-07 |
| **M5 — DuplicateLens (exact)** | Three-pass pipeline, review UI, auto-select, hardlink/reflink | AC-08 to AC-10 |
| **M6 — DuplicateLens (visual)** | Image perceptual hashing, similarity slider; video (optional) | Precision ≥ 95% on labelled test set at default threshold |
| **M7 — Polish & 1.0** | Scan cache, watch mode, exports, accessibility pass, installers, signing | All M-priority requirements verified |

---

## Appendix A — DevSweep Detection Rules (initial set)

| Ecosystem | Marker | Artifact path(s) | Risk | Regenerate with |
| --- | --- | --- | --- | --- |
| Rust | `Cargo.toml` | `target/` | Safe | `cargo build` |
| Node.js | `package.json` | `node_modules/` | Safe (lockfile present) / Caution (no lockfile) | `npm ci` / `pnpm i` / `yarn` |
| Next.js | `next.config.*` | `.next/` | Safe | `next build` |
| Nuxt | `nuxt.config.*` | `.nuxt/`, `.output/` | Safe | `nuxt build` |
| Vite / generic JS | `vite.config.*` | `dist/` | Caution | `vite build` |
| Turborepo | `turbo.json` | `.turbo/` | Safe | automatic |
| Python | `pyvenv.cfg` (inside dir) | `venv/`, `.venv/`, any dir containing `pyvenv.cfg` | Review | `python -m venv` + install |
| Python | any `.py` | `__pycache__/`, `.pytest_cache/`, `.mypy_cache/`, `.ruff_cache/`, `.tox/` | Safe | automatic |
| Java (Maven) | `pom.xml` | `target/` | Safe | `mvn package` |
| Gradle | `build.gradle*`, `settings.gradle*` | `build/`, `.gradle/` | Safe | `gradle build` |
| Xcode | `*.xcodeproj`, `*.xcworkspace` | `~/Library/Developer/Xcode/DerivedData/<project>-*` | Safe | Xcode build |
| CocoaPods | `Podfile` | `Pods/` | Caution | `pod install` |
| Flutter/Dart | `pubspec.yaml` | `build/`, `.dart_tool/` | Safe | `flutter pub get` |
| Go | `go.mod` | (none per-project; see global caches) | — | — |
| .NET | `*.csproj`, `*.sln` | `bin/`, `obj/` | Safe | `dotnet build` |
| C/C++ (CMake) | `CMakeLists.txt` | `build/`, `cmake-build-*/` | Caution | `cmake --build` |
| Zig | `build.zig` | `zig-cache/`, `.zig-cache/`, `zig-out/` | Safe | `zig build` |
| Haskell | `stack.yaml` | `.stack-work/` | Safe | `stack build` |
| Elixir | `mix.exs` | `_build/`, `deps/` | Safe | `mix deps.get && mix compile` |
| Terraform | `*.tf` | `.terraform/` | Caution | `terraform init` |
| Unity | `ProjectSettings/ProjectVersion.txt` | `Library/`, `Temp/`, `obj/` | Caution (slow reimport) | Open in Unity |

**Global caches (per-user):**

| Cache | Default location(s) | Risk |
| --- | --- | --- |
| Cargo registry & git | `~/.cargo/registry`, `~/.cargo/git` | Caution (re-download) |
| npm | `~/.npm` (Unix), `%LocalAppData%\npm-cache` (Windows) | Safe |
| pnpm store | platform-specific pnpm store path | Caution (breaks links until reinstall) |
| Yarn | Yarn cache dir | Safe |
| pip | `~/.cache/pip`, `~/Library/Caches/pip`, `%LocalAppData%\pip\Cache` | Safe |
| Gradle | `~/.gradle/caches` | Caution |
| Maven | `~/.m2/repository` | Caution |
| Go modules | `$GOMODCACHE` (default `~/go/pkg/mod`) | Caution (read-only files; use `go clean -modcache`) |
| Xcode | `~/Library/Developer/Xcode/DerivedData`, `iOS DeviceSupport`, `~/Library/Developer/CoreSimulator/Caches` | Safe / Caution |
| Docker | via Docker API only | Per resource type |

## Appendix B — Keyboard Shortcuts (default)

| Key | Action |
| --- | --- |
| Ctrl/Cmd + 1/2/3 | Switch module |
| Arrow keys | Move selection in chart / list |
| Enter | Zoom into selected directory |
| Backspace | Zoom out one level |
| Delete | Stage selection in Trash Drawer |
| Ctrl/Cmd + F | Search |
| Ctrl/Cmd + R | Rescan current root |
| Esc | Cancel running job / close dialog |
| ? | Shortcut overlay |

## Appendix C — GUI Framework Decision

| Criterion | `eframe` / `egui` | `iced` |
| --- | --- | --- |
| Custom 2D drawing (arcs, meshes) | Direct `Painter` and `Mesh` access; straightforward | `Canvas` widget with caching; capable |
| Large-list performance | `egui_extras::TableBuilder` virtualised rows | Requires manual virtualisation |
| Accessibility | AccessKit integrated | AccessKit support maturing |
| Architecture | Immediate mode; state lives in app struct | Elm-style message/update; clean for complex state |
| Native look | Custom look on all platforms | Custom look on all platforms |
| Decision | **Selected** for faster iteration on the custom chart and virtualised tables | Documented fallback |

## Appendix D — Open Questions

1. Product naming: the DevSweep module was unnamed in the concept; confirm "DevSweep" or choose another name, and whether the three modules ship as one app or three.
2. Licence: open source (MIT/Apache-2.0 dual) or proprietary? This affects the use of any GPL-licensed decoders.
3. HEIC and camera RAW support: rely on system codecs (Windows WIC, macOS ImageIO) or bundle `libheif`/`rawloader`?
4. Should the Windows MFT fast path (FR-SCAN-15) be in 1.0, given it requires elevation?
5. Should "express mode" (FR-DEV-12) exist at all, or should every deletion go through the Trash Drawer?
6. Pricing/distribution channels (Mac App Store sandboxing would restrict scanning and needs evaluation).
