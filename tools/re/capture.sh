#!/usr/bin/env bash
# Capture the Kone Pro Air's USB traffic with usbmon.
#
# Usage: tools/re/capture.sh <name>                capture until Ctrl+C
#        tools/re/capture.sh --background <name>   capture in the background
#        tools/re/capture.sh --stop                stop the background capture and
#                                                  print its config-channel extract
#
# Writes tools/re/captures/<NN>-<name>.pcapng plus a .dev file describing the
# device (bus, address, config interface and its endpoints) for extract.sh.
# With both connected the dongle is captured; KONECTL_RE_DEVICE=cable (or
# dongle) picks one.
# Needs usbmon loaded and read access to /dev/usbmon* (see the host setup in
# docs/reverse-engineering.md).
set -euo pipefail

readonly VENDOR_ID="1e7d"
# Preferred order: wireless dongle first (that is what the VM gets), then cable.
case "${KONECTL_RE_DEVICE:-}" in
    "") PRODUCT_IDS=("2c8e" "2c92") ;;
    dongle) PRODUCT_IDS=("2c8e") ;;
    cable) PRODUCT_IDS=("2c92") ;;
    *)
        echo "error: KONECTL_RE_DEVICE must be dongle or cable" >&2
        exit 1
        ;;
esac
readonly PRODUCT_IDS

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
readonly CAPTURE_DIR="${SCRIPT_DIR}/captures"
# Background capture: dumpcap's PID on the first line, the output file on the second.
readonly STATE_FILE="${CAPTURE_DIR}/.background"

die() {
    echo "error: $*" >&2
    exit 1
}

usage() {
    die "usage: $0 [--background] <name> (lowercase, digits, dashes) | $0 --stop"
}

stop_background() {
    local pid output
    [[ -f "$STATE_FILE" ]] || die "no background capture is running"
    { read -r pid && read -r output; } <"$STATE_FILE"
    # Only signal the process if it still is our dumpcap: the PID may have been
    # reused after a crash or a reboot.
    if [[ "$(cat "/proc/${pid}/comm" 2>/dev/null)" == dumpcap ]]; then
        kill -INT "$pid"
        for _ in $(seq 100); do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.1
        done
        if kill -0 "$pid" 2>/dev/null; then
            die "dumpcap (PID ${pid}) did not stop"
        fi
    fi
    rm -f -- "$STATE_FILE"
    echo "stopped: ${output}"
    [[ -f "$output" ]] || die "the capture file ${output} is missing"
    "${SCRIPT_DIR}/extract.sh" "$output"
}

background=false
case "${1:-}" in
    --stop)
        [[ $# -eq 1 ]] || usage
        stop_background
        exit 0
        ;;
    --background)
        [[ $# -eq 2 ]] || usage
        background=true
        name="$2"
        ;;
    *)
        [[ $# -eq 1 ]] || usage
        name="$1"
        ;;
esac
[[ "$name" =~ ^[a-z0-9][a-z0-9-]*$ ]] || usage
if [[ -f "$STATE_FILE" ]]; then
    die "a background capture is still running; stop it with: $0 --stop"
fi

# Prints "<sysfs device dir> <product id>" of the first matching device.
find_device() {
    local product_id device_dir
    for product_id in "${PRODUCT_IDS[@]}"; do
        for device_dir in /sys/bus/usb/devices/*; do
            [[ -f "$device_dir/idVendor" && -f "$device_dir/idProduct" ]] || continue
            if [[ "$(<"$device_dir/idVendor")" == "$VENDOR_ID" &&
                "$(<"$device_dir/idProduct")" == "$product_id" ]]; then
                echo "$device_dir $product_id"
                return 0
            fi
        done
    done
    return 1
}

read -r device_dir product_id < <(find_device) ||
    die "no Kone Pro Air connected${KONECTL_RE_DEVICE:+ (${KONECTL_RE_DEVICE})}"
case "$product_id" in
    2c8e) interface=2 ;; # dongle
    2c92) interface=1 ;; # cable
esac
bus="$(<"$device_dir/busnum")"
address="$(<"$device_dir/devnum")"

interface_dir="$(find "$device_dir/" -maxdepth 1 -name "*:1.${interface}" -print -quit)"
[[ -n "$interface_dir" ]] || die "interface $interface not found under $device_dir"
endpoints="$(cat "$interface_dir"/ep_*/bEndpointAddress | paste -sd, -)"

[[ -e "/dev/usbmon${bus}" ]] || die "usbmon is not loaded (sudo modprobe usbmon)"
[[ -r "/dev/usbmon${bus}" ]] ||
    die "no read access to /dev/usbmon${bus} (see docs/reverse-engineering.md, host setup)"

mkdir -p "$CAPTURE_DIR"
# Next number after the highest existing one, so deleted captures leave gaps
# instead of numbers being reused.
last="$(find "$CAPTURE_DIR" -maxdepth 1 -name '[0-9][0-9]-*.pcapng' -printf '%f\n' |
    cut -c1-2 | sort -n | tail -n 1)"
number=0
if [[ -n "$last" ]]; then
    number=$((10#$last + 1))
fi
output="${CAPTURE_DIR}/$(printf '%02d' "$number")-${name}.pcapng"
[[ ! -e "$output" ]] || die "${output} already exists"

printf 'bus=%s address=%s product=%s interface=%s endpoints=%s\n' \
    "$bus" "$address" "$product_id" "$interface" "$endpoints" >"${output%.pcapng}.dev"

echo "capturing usbmon${bus} (device ${bus}:${address}, ${VENDOR_ID}:${product_id}) → ${output}"
if ! "$background"; then
    echo "stop with Ctrl+C"
    exec dumpcap -q -i "usbmon${bus}" -w "$output"
fi

setsid dumpcap -q -i "usbmon${bus}" -w "$output" >"${output%.pcapng}.log" 2>&1 &
pid=$!
# Wait until dumpcap has opened its output file, so no traffic is missed.
for _ in $(seq 50); do
    [[ -s "$output" ]] && break
    kill -0 "$pid" 2>/dev/null || die "dumpcap exited, see ${output%.pcapng}.log"
    sleep 0.1
done
if [[ ! -s "$output" ]]; then
    kill -INT "$pid" 2>/dev/null || true
    die "dumpcap did not start, see ${output%.pcapng}.log"
fi
# Written only once dumpcap runs, so a failed start leaves no stale state.
printf '%s\n%s\n' "$pid" "$output" >"$STATE_FILE"
echo "running in the background; stop with: $0 --stop"
