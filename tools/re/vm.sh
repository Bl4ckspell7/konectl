#!/usr/bin/env bash
# Drive the capture VM from the host: screenshots, mouse clicks and key presses.
#
#   tools/re/vm.sh shot [FILE]                 screenshot (default: tools/re/captures/screen.png)
#   tools/re/vm.sh move X Y                    pointer to pixel X,Y
#   tools/re/vm.sh click X Y [right|double]    click at pixel X,Y
#   tools/re/vm.sh scroll X Y STEPS            mouse wheel at X,Y; negative steps scroll up
#   tools/re/vm.sh key KEY...                  QEMU key names, e.g. ret, tab, esc, ctrl-a, meta_l-up
#   tools/re/vm.sh type TEXT                   type ASCII text
#
# Pixel coordinates refer to the VM screen (1280x800 by default). Needs the QMP
# socket that tools/re/compose.yaml configures.
set -euo pipefail

readonly CONTAINER="konectl-re-windows"
readonly WIDTH=1280 HEIGHT=800
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR

die() {
    echo "error: $*" >&2
    exit 1
}

usage() {
    # The comment block at the top of this file.
    awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "${BASH_SOURCE[0]}"
    exit 2
}

# Runs commands on QEMU's human monitor (HMP) and fails on monitor errors,
# e.g. an unknown key name.
hmp() {
    local output errors
    output="$(printf '%s\n' "$@" | docker exec -i "$CONTAINER" sh -c 'nc -U -q 1 /run/shm/monitor.sock')"
    errors="$(grep -iE 'unknown|error|invalid|could not' <<<"$output" || true)"
    [[ -z "$errors" ]] || die "QEMU monitor: ${errors%%$'\n'*}"
}

# Sends QMP commands (JSON strings) through the socket inside the container.
readonly QMP_CLIENT='
import json
import socket
import sys
import time
from typing import Any, TextIO

sock: socket.socket = socket.socket(socket.AF_UNIX)
sock.connect("/run/shm/qmp.sock")
stream: TextIO = sock.makefile("rw")


def reply() -> dict[str, Any]:
    # Next reply, skipping asynchronous events.
    while True:
        message: dict[str, Any] = json.loads(stream.readline())
        if "event" not in message:
            return message


reply()
commands: list[str] = [json.dumps({"execute": "qmp_capabilities"})] + sys.argv[1:]
for command in commands:
    stream.write(command + "\n")
    stream.flush()
    answer: dict[str, Any] = reply()
    if "error" in answer:
        sys.exit(str(answer))
    time.sleep(0.05)
'

qmp() {
    docker exec -i "$CONTAINER" python3 -c "$QMP_CLIENT" "$@"
}

input_events() {
    printf '{"execute":"input-send-event","arguments":{"events":[%s]}}' "$1"
}

# Fails unless X,Y is a pixel on the VM screen.
check_point() {
    [[ "$1" =~ ^[0-9]+$ && "$2" =~ ^[0-9]+$ ]] || die "coordinates must be whole numbers: $1,$2"
    ((10#$1 < WIDTH && 10#$2 < HEIGHT)) || die "point $1,$2 is outside the ${WIDTH}x${HEIGHT} screen"
}

# The emulated tablet takes absolute coordinates scaled to 0..32767.
abs_move() {
    check_point "$1" "$2"
    local x=$((10#$1 * 32767 / (WIDTH - 1))) y=$((10#$2 * 32767 / (HEIGHT - 1)))
    qmp "$(input_events "{\"type\":\"abs\",\"data\":{\"axis\":\"x\",\"value\":${x}}},{\"type\":\"abs\",\"data\":{\"axis\":\"y\",\"value\":${y}}}")"
}

button() {
    local name="$1" press release
    press="$(input_events "{\"type\":\"btn\",\"data\":{\"down\":true,\"button\":\"${name}\"}}")"
    release="$(input_events "{\"type\":\"btn\",\"data\":{\"down\":false,\"button\":\"${name}\"}}")"
    if [[ "${2:-}" == double ]]; then
        qmp "$press" "$release" "$press" "$release"
    else
        qmp "$press" "$release"
    fi
}

# QEMU key name for one ASCII character.
char_key() {
    local c="$1"
    case "$c" in
        [a-z0-9]) echo "$c" ;;
        [A-Z]) echo "shift-${c,,}" ;;
        ' ') echo spc ;;
        '.') echo dot ;;
        ',') echo comma ;;
        '/') echo slash ;;
        \\) echo backslash ;;
        ':') echo shift-semicolon ;;
        ';') echo semicolon ;;
        '-') echo minus ;;
        '_') echo shift-minus ;;
        '=') echo equal ;;
        '?') echo shift-slash ;;
        '&') echo shift-7 ;;
        '#') echo shift-3 ;;
        '"') echo shift-apostrophe ;;
        "'") echo apostrophe ;;
        *) die "unsupported character: $c" ;;
    esac
}

[[ $# -ge 1 ]] || usage
case "$1" in
    shot)
        output="${2:-${SCRIPT_DIR}/captures/screen.png}"
        mkdir -p "$(dirname "$output")"
        # Remove the last screenshot first, so a failed screendump cannot return it.
        docker exec "$CONTAINER" rm -f /tmp/screen.png
        hmp "screendump /tmp/screen.png -f png"
        docker exec "$CONTAINER" test -s /tmp/screen.png || die "screendump failed"
        docker cp -q "${CONTAINER}:/tmp/screen.png" "$output"
        echo "$output"
        ;;
    move)
        [[ $# -eq 3 ]] || usage
        abs_move "$2" "$3"
        ;;
    click)
        [[ $# -ge 3 ]] || usage
        abs_move "$2" "$3"
        case "${4:-left}" in
            left) button left ;;
            right) button right ;;
            double) button left double ;;
            *) usage ;;
        esac
        ;;
    scroll)
        [[ $# -eq 4 && "$4" =~ ^-?[0-9]+$ ]] || usage
        abs_move "$2" "$3"
        # Decimal even with leading zeros (08 would be read as octal).
        steps=$((10#${4#-})) direction="wheel-down"
        if [[ "$4" == -* ]]; then
            direction="wheel-up"
        fi
        for ((i = 0; i < steps; i++)); do button "$direction"; done
        ;;
    key)
        shift
        [[ $# -ge 1 ]] || usage
        for key in "$@"; do hmp "sendkey $key"; done
        ;;
    type)
        [[ $# -eq 2 ]] || usage
        text="$2"
        commands=()
        for ((i = 0; i < ${#text}; i++)); do
            commands+=("sendkey $(char_key "${text:i:1}")")
        done
        hmp "${commands[@]}"
        ;;
    *) usage ;;
esac
