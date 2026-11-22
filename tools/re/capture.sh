#!/usr/bin/env bash
# Capture the Kone Pro Air's USB traffic with usbmon.
#
# Usage: tools/re/capture.sh <name>        stop with Ctrl+C
#
# Writes tools/re/captures/<NN>-<name>.pcapng plus a .dev file describing the
# device (bus, address, config interface and its endpoints) for extract.sh.
# Needs usbmon loaded and read access to /dev/usbmon* (see the host setup in
# docs/reverse-engineering.md).
set -euo pipefail

readonly VENDOR_ID="1e7d"
# Preferred order: wireless dongle first (that is what the VM gets), then cable.
readonly PRODUCT_IDS=("2c8e" "2c92")

die() {
    echo "error: $*" >&2
    exit 1
}

[[ $# -eq 1 && "$1" =~ ^[a-z0-9][a-z0-9-]*$ ]] || die "usage: $0 <name> (lowercase, digits, dashes)"
name="$1"

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

read -r device_dir product_id < <(find_device) || die "no Kone Pro Air connected"
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

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
capture_dir="${script_dir}/captures"
mkdir -p "$capture_dir"
# Next number after the highest existing one, so deleted captures leave gaps
# instead of numbers being reused.
last="$(find "$capture_dir" -maxdepth 1 -name '[0-9][0-9]-*.pcapng' -printf '%f\n' |
    cut -c1-2 | sort -n | tail -n 1)"
number=0
if [[ -n "$last" ]]; then
    number=$((10#$last + 1))
fi
output="${capture_dir}/$(printf '%02d' "$number")-${name}.pcapng"
[[ ! -e "$output" ]] || die "${output} already exists"

printf 'bus=%s address=%s product=%s interface=%s endpoints=%s\n' \
    "$bus" "$address" "$product_id" "$interface" "$endpoints" >"${output%.pcapng}.dev"

echo "capturing usbmon${bus} (device ${bus}:${address}, ${VENDOR_ID}:${product_id}) → ${output}"
echo "stop with Ctrl+C"
exec dumpcap -q -i "usbmon${bus}" -w "$output"
