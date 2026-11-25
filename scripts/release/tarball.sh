#!/usr/bin/env bash
# Packs the release binaries for the GitHub release:
#
#   tarball.sh <version>
#
# writes dist/konectl-<version>-<arch>-linux.tar.gz and its .sha256 file, from
# the binaries of `cargo build --release --locked -p konectl-cli -p konectl-gui`.
# The archive holds bin/ and share/ (desktop file, AppStream metadata, icon),
# so it can be unpacked into ~/.local or /usr/local as it is.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

readonly APP_ID="io.github.bl4ckspell7.konectl"
readonly DATA="crates/konectl-gui/data"

die() {
    echo "error: $*" >&2
    exit 1
}

version="${1:-}"
[[ "$version" =~ ^[0-9A-Za-z.+-]+$ ]] || die "usage: $0 <version>"
name="konectl-${version}-$(uname -m)-linux"

# Where cargo put the binaries (CARGO_TARGET_DIR or a configured target-dir).
target_dir="$(cargo metadata --format-version 1 --no-deps |
    sed -nE 's/.*"target_directory":"([^"]+)".*/\1/p')"
[[ -n "$target_dir" ]] || die "cannot find cargo's target directory"
release="${target_dir}/release"
for binary in konectl konectl-gui; do
    [[ -x "${release}/${binary}" ]] || die "${release}/${binary} is missing: build the release first"
done

stage="dist/${name}"
rm -rf -- "${stage:?}"
install -Dm0755 "${release}/konectl" "${stage}/bin/konectl"
install -Dm0755 "${release}/konectl-gui" "${stage}/bin/konectl-gui"
install -Dm0644 "${DATA}/${APP_ID}.desktop" "${stage}/share/applications/${APP_ID}.desktop"
install -Dm0644 "${DATA}/${APP_ID}.metainfo.xml" "${stage}/share/metainfo/${APP_ID}.metainfo.xml"
install -Dm0644 "${DATA}/icons/${APP_ID}.svg" \
    "${stage}/share/icons/hicolor/scalable/apps/${APP_ID}.svg"
install -Dm0644 -t "${stage}" README.md LICENSE

tar -C dist --owner=0 --group=0 --numeric-owner -czf "dist/${name}.tar.gz" "${name}"
(cd dist && sha256sum "${name}.tar.gz" >"${name}.tar.gz.sha256")
echo "dist/${name}.tar.gz"
