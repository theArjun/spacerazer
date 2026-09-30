# Contributing to SpaceRazer

Thanks for helping. Bug reports, fixes, new DevSweep rules and platform
testing are all welcome.

## Getting set up

Install [Rust](https://rustup.rs) 1.85 or newer (and on Linux the libraries
listed in the README), then:

```sh
make dev     # debug build and launch
make test    # run every test
make lint    # rustfmt check and clippy with warnings as errors
```

CI runs `make lint`'s checks and the tests on Linux, macOS and Windows for
every pull request.

## Ground rules

- **Safety first.** SpaceRazer deletes files. Anything that writes, moves,
  links or deletes must go through `sr-ops`, which revalidates each item
  just before acting and never follows symlinks. Changes there need tests,
  including a property test when the behaviour depends on input shape.
- **Keep the core GUI-free.** Only `sr-gui` may depend on egui.
- **No network access and no telemetry.**
- **`unsafe` only for platform FFI or memory mapping**, isolated in small
  functions with a `// SAFETY:` comment.
- Follow the style around the code you touch; `cargo fmt` settles formatting.

## Adding a DevSweep rule

Rules are data in `crates/sr-devsweep/src/rules.toml`. Each rule names the
marker files that identify a project, the artifact folders next to them, a
risk level and how to regenerate them. Add a test in
`crates/sr-devsweep/src/tests.rs` showing the folder is flagged with its
marker and ignored without it.

## Screenshots

`SPACERAZER_SCREENSHOT=out.png` makes the app save a screenshot of its window
after `SPACERAZER_SCREENSHOT_DELAY` seconds (default 5) and quit.
`SPACERAZER_TAB=dev` or `dup` opens that tool and runs it on the folder given
as the argument. The README images are taken this way from a demo folder, so
they contain no personal paths.

## Releasing

1. Update `version` in `[workspace.package]` in `Cargo.toml` and add the
   changes to `CHANGELOG.md`.
2. Commit, then tag and push:

   ```sh
   git tag v0.2.0
   git push origin v0.2.0
   ```

3. The Release workflow builds for Linux (x86_64, aarch64), Windows (x86_64)
   and macOS (universal), and publishes a GitHub Release with checksums. The
   tag must match the version in `Cargo.toml`.

macOS builds are notarized when the `APPLE_*` secrets described at the top of
`.github/workflows/release.yml` are set; otherwise they are ad-hoc signed.
