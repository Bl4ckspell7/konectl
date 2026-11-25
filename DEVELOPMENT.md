# Development

## Prerequisites

- Rust 1.88 or newer (edition 2024); the GUI (`crates/konectl-gui`, Slint) needs 1.95
- a C compiler, `pkg-config` and the libudev development files (see [Install](README.md#install))
- for the GUI: the fontconfig and freetype development files (Arch: `fontconfig freetype2`,
  Debian/Ubuntu: `libfontconfig-dev libfreetype-dev`)
- For protocol work: [docs/reverse-engineering.md](docs/reverse-engineering.md)

## Checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
shellcheck tools/re/*.sh scripts/release/*.sh
```

The **Rust** workflow (`.github/workflows/rust.yml`) runs the same checks on every push
and pull request to `main`, plus `cargo check` with the minimum supported Rust versions
(`rust-version` in `Cargo.toml`, and in `crates/konectl-gui/Cargo.toml` for the GUI).

## GUI

`konectl-gui` runs all mouse access on a worker thread (`src/worker.rs`); the window only
sends requests and shows events. Without a mouse:

```sh
KONECTL_GUI_DEMO=1 cargo run -p konectl-gui          # a simulated mouse
KONECTL_GUI_DEMO=no-access cargo run -p konectl-gui  # also: asleep, unplugged
SLINT_STYLE=cosmic cargo run -p konectl-gui          # another widget style (default fluent)
SLINT_BACKEND=winit-software cargo run -p konectl-gui  # without OpenGL
```

`cargo run -p konectl-gui --features screenshots -- --screenshots DIR` draws the demo
screens into PPM files without opening a window.

To inspect and drive the running GUI without a window (Slint's embedded MCP server, for
AI agents and scripted checks):

```sh
SLINT_EMIT_DEBUG_INFO=1 SLINT_MCP_PORT=8791 SLINT_BACKEND=headless \
    cargo run -p konectl-gui --features slint/mcp
```

It serves MCP over HTTP on `127.0.0.1:8791` (`/mcp`): element tree, screenshots, clicks,
key events. Works on the real mouse too; add `KONECTL_GUI_DEMO=1` for the demo.

With the mouse connected and awake, `cargo test -p konectl-gui -- --ignored
--test-threads=1` reads it and writes a few changes, then undoes them and checks that the
mouse is as before.

## Logging

The library, the CLI and the GUI log through the `log` crate; the programs print to stderr
with env_logger, filtered by `RUST_LOG`:

| Level   | What                                                                      |
|---------|---------------------------------------------------------------------------|
| `info`  | GUI start, connection states, each change as its `konectl` command, notes |
| `warn`  | refused and failed changes, other errors                                  |
| `debug` | each GUI request, devices opened, requests sent again, read-back retries  |
| `trace` | every report sent to (`→`) and read from (`←`) the mouse, GUI polling     |

The GUI logs `info` by default, the CLI only warnings. Examples:

```sh
RUST_LOG=konectl=debug cargo run -p konectl-gui
RUST_LOG=konectl=trace konectl settings          # the reports of one CLI command
flatpak run --env=RUST_LOG=konectl=trace io.github.bl4ckspell7.konectl
```

`konectl` also matches `konectl_gui`; a plain `RUST_LOG=debug` also turns on the debug
output of every dependency. Started from the desktop, the GUI's output goes to the session
journal (`journalctl --user -f` on systemd desktops). Logs show device and file paths, but
no serial numbers; macro keys only appear in the `trace` reports.

## Packaging

The **Package** workflow (`.github/workflows/package.yml`) runs on pull requests and for
every release, which attaches its files to the GitHub release:

- a tarball with both programs, the desktop file, the AppStream metadata and the icon
  (`scripts/release/tarball.sh`), built in a Debian 12 container, so it runs with glibc
  2.36 or newer;
- a Flatpak bundle of the GUI (`io.github.bl4ckspell7.konectl.yml`, runtime
  `org.freedesktop.Platform` 26.08). The build runs offline: the workflow first turns
  `Cargo.lock` into `cargo-sources.json` with flatpak-cargo-generator (not committed).

The desktop file, metadata and icon are in `crates/konectl-gui/data/`. Their app ID,
`io.github.bl4ckspell7.konectl`, is also the Wayland app ID the GUI sets
(`src/main.rs`). The release bump adds the new version to the metadata's `<releases>`.

Locally:

```sh
cargo build --release --locked -p konectl-cli -p konectl-gui
scripts/release/tarball.sh 0.0.0-test     # → dist/konectl-0.0.0-test-x86_64-linux.tar.gz

uvx --from flatpak-cargo-generator flatpak-cargo-generator Cargo.lock -o cargo-sources.json
flatpak-builder --user --install-deps-from=flathub --force-clean flatpak-build \
    io.github.bl4ckspell7.konectl.yml
flatpak-builder --run flatpak-build io.github.bl4ckspell7.konectl.yml konectl-gui
```

The README screenshots (`docs/screenshots/`) come from the demo mouse: run
`--screenshots DIR` (see [GUI](#gui)) and convert the PPM files to PNG.

## Dependencies

Renovate (`renovate.json`, `.github/workflows/renovate.yml`) runs once a day and opens
pull requests for Cargo dependencies, GitHub Actions, the CI container images, the VM
image in `tools/re/compose.yaml`, git-cliff and flatpak-cargo-generator. New releases wait
5 days, `Cargo.lock` is refreshed on Mondays, and the dependency dashboard issue lists
everything pending.

It needs the `RENOVATE_TOKEN` secret: a fine-grained personal access token for this
repository with read and write access to contents, commit statuses, issues, pull requests
and workflows, and read access to Dependabot alerts.

## Releasing

Releases are cut by the **Release** workflow (`.github/workflows/release.yml`), which is
only triggered by hand. Commit messages must follow
[Conventional Commits](https://www.conventionalcommits.org); the version bump and the
changelog are derived from them.

1. **Actions → Release → Run workflow**, and pick the bump:
   - `auto` (default): derived from the commits since the last tag (`feat` → minor,
     `fix` → patch; while the version is 0.x, a breaking change bumps the minor version).
   - `patch` / `minor` / `major`: force a level.

   Leave **`dry_run` checked** the first time.
2. **Review the dry-run job summary.** It shows the computed version and the release
   notes, and the job checks that all crates package.
3. **Run it again with `dry_run` unchecked** (from `main`; other branches are refused).
   The `release` job bumps the version (`Cargo.toml`, `Cargo.lock`, the GUI's AppStream
   metadata), updates `CHANGELOG.md`, runs the tests, then commits
   `chore(release): vX.Y.Z` and pushes it together with the tag.
4. The `publish` job publishes `konectl`, then `konectl-cli` and `konectl-gui`, to
   crates.io with Trusted
   Publishing through
   [crates-io-auth-action](https://github.com/rust-lang/crates-io-auth-action) (no stored
   token). It runs in the `crates-io` environment; approve it there if the environment
   requires a reviewer. Versions already on crates.io are skipped, so if it fails, re-run
   the failed jobs.
5. The `package` job builds the tarball and the Flatpak bundle from the tag (the
   **Package** workflow).
6. The `github-release` job opens a **draft** GitHub release with the release notes and
   the packages. **Review it, then publish it.**

### One-time setup

- crates.io, for each crate: **Settings → Trusted Publishing → GitHub**, with owner
  `Bl4ckspell7`, repository `konectl`, workflow `release.yml` and environment `crates-io`.
  This is only possible once the crate exists, so a new crate (like `konectl-gui` in
  0.4.0) fails in the `publish` job the first time: publish it by hand from the release
  tag (`git switch --detach vX.Y.Z && cargo publish -p konectl-gui`, with an API token),
  set up Trusted Publishing for it, then re-run the failed jobs.
- GitHub: **Settings → Environments → New environment** `crates-io` (optionally with a
  required reviewer).

### Preview locally (optional)

```sh
git cliff --bumped-version             # next version
git cliff --unreleased                 # notes for the upcoming release
scripts/release/bump.sh compute auto   # what the workflow will pick
```
