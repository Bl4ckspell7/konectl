#!/usr/bin/env bash
# Download the Windows 11 LTSC evaluation ISO for the capture VM (resumable).
#
# Usage: tools/re/fetch-iso.sh             re-run to resume an interrupted download
#
# Saves to ${KONECTL_RE_DIR:-~/.local/share/konectl-re}/win11-ltsc-eval.iso, the
# path compose.yaml mounts. URL, size and checksum are the ones dockur/windows
# uses for VERSION=11l.
set -euo pipefail

readonly URL="https://software-static.download.prss.microsoft.com/dbazure/888969d5-f34g-4e03-ac9d-1f9786c66749/26100.1742.240906-0331.ge_release_svc_refresh_CLIENT_LTSC_EVAL_x64FRE_en-us.iso"
readonly SIZE=5112850432
readonly SHA256="67cec5865eaa037a72ddc633a717a10a2bed50778862267223ddb9c60ef5da68"

die() {
    echo "error: $*" >&2
    exit 1
}

directory="${KONECTL_RE_DIR:-${HOME}/.local/share/konectl-re}"
iso="${directory}/win11-ltsc-eval.iso"
mkdir -p "$directory"

current_size() {
    if [[ -f "$iso" ]]; then stat -c %s "$iso"; else echo 0; fi
}

if (($(current_size) < SIZE)); then
    echo "downloading → ${iso}"
    curl --fail --location --continue-at - --retry 10 --retry-all-errors --retry-delay 5 \
        --output "$iso" "$URL"
fi

(($(current_size) == SIZE)) || die "unexpected size $(current_size), expected ${SIZE}; delete ${iso} and retry"

echo "verifying checksum…"
echo "${SHA256}  ${iso}" | sha256sum --check --quiet || die "checksum mismatch; delete ${iso} and retry"
echo "ok: ${iso}"
