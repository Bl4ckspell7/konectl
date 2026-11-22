#!/usr/bin/env bash
# Print the configuration-channel traffic of a capture, one report per line:
#
#   <seconds>  OUT|IN  <hex bytes, trailing zeros trimmed>
#
# Usage: tools/re/extract.sh [--full] <capture.pcapng>
#
# --full prints all 64 bytes of every report, without trimming (some blocks
# carry data past their declared length).
#
# Uses the .dev file written by capture.sh to select the mouse and its config
# interface. Interrupt transfers on the config endpoints and HID class control
# requests (SET_REPORT / GET_REPORT) are included; mouse movement is not.
set -euo pipefail

die() {
    echo "error: $*" >&2
    exit 1
}

full=0
if [[ "${1:-}" == --full ]]; then
    full=1
    shift
fi
[[ $# -eq 1 ]] || die "usage: $0 [--full] <capture.pcapng>"
capture="$1"
device_file="${capture%.pcapng}.dev"
[[ -f "$capture" ]] || die "no such capture: $capture"
[[ -f "$device_file" ]] || die "missing $device_file (written by capture.sh)"

# Reads bus=, address=, interface=, endpoints= from the .dev file.
bus="" address="" interface="" endpoints=""
read -r -a fields <"$device_file"
for field in "${fields[@]}"; do
    case "$field" in
        bus=*) bus="${field#bus=}" ;;
        address=*) address="${field#address=}" ;;
        interface=*) interface="${field#interface=}" ;;
        endpoints=*) endpoints="${field#endpoints=}" ;;
    esac
done
[[ -n "$bus" && -n "$address" && -n "$interface" && -n "$endpoints" ]] || die "incomplete $device_file"

endpoint_set="$(sed 's/,/, 0x/g; s/^/0x/' <<<"$endpoints")"
filter="usb.bus_id == ${bus} && usb.device_address == ${address} && usb.data_len > 0 && (
    (usb.transfer_type == 0x01 && usb.endpoint_address in {${endpoint_set}})
    || (usb.transfer_type == 0x02 && usb.endpoint_address.number == 0)
)"

tshark -r "$capture" -Y "$filter" -T fields -E separator=';' \
    -e frame.time_relative \
    -e usb.endpoint_address.direction \
    -e usb.setup.wIndex \
    -e usb.capdata \
    -e usb.data_fragment \
    -e usbhid.data |
    awk -F';' -v interface="$interface" -v full="$full" '
        {
            data = $4 != "" ? $4 : ($5 != "" ? $5 : $6)
            if (data == "") next
            # Control requests must target the config interface (wIndex); completions carry none.
            if ($3 != "" && $3 + 0 != interface + 0) next
            gsub(/[: ]/, "", data)
            if (!full) {
                untrimmed = data
                sub(/(00)+$/, "", data)
                # Keep at least the two command bytes, e.g. "90 00".
                if (length(data) < 4) data = substr(untrimmed, 1, 4)
            }
            hex = ""
            for (i = 1; i <= length(data); i += 2) hex = hex (i > 1 ? " " : "") substr(data, i, 2)
            printf "%10.6f  %-3s  %s\n", $1, ($2 == "1" ? "IN" : "OUT"), hex
        }'
