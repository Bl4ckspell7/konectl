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

## Dependencies

Renovate (`renovate.json`, `.github/workflows/renovate.yml`) runs once a day and opens
pull requests for Cargo dependencies, GitHub Actions, the CI container image, the VM
image in `tools/re/compose.yaml` and git-cliff. New releases wait 5 days, `Cargo.lock` is
refreshed on Mondays, and the dependency dashboard issue lists everything pending.

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
   notes, and the job checks that both crates package.
3. **Run it again with `dry_run` unchecked** (from `main`; other branches are refused).
   The `release` job bumps the version (`Cargo.toml`, `Cargo.lock`), updates
   `CHANGELOG.md`, runs the tests, then commits
   `chore(release): vX.Y.Z` and pushes it together with the tag.
4. The `publish` job publishes `konectl`, then `konectl-cli`, to crates.io with Trusted
   Publishing through
   [crates-io-auth-action](https://github.com/rust-lang/crates-io-auth-action) (no stored
   token). It runs in the `crates-io` environment; approve it there if the environment
   requires a reviewer. Versions already on crates.io are skipped, so if it fails, re-run
   the failed jobs.
5. The `github-release` job opens a **draft** GitHub release with the release notes.
   **Review it, then publish it.**

### One-time setup

- crates.io, for both crates: **Settings → Trusted Publishing → GitHub**, with owner
  `Bl4ckspell7`, repository `konectl`, workflow `release.yml` and environment `crates-io`.
- GitHub: **Settings → Environments → New environment** `crates-io` (optionally with a
  required reviewer).

### Preview locally (optional)

```sh
git cliff --bumped-version             # next version
git cliff --unreleased                 # notes for the upcoming release
scripts/release/bump.sh compute auto   # what the workflow will pick
```
