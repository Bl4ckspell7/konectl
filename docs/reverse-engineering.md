# Reverse-engineering the configuration protocol

How the settings protocol in [protocol.md](protocol.md) was worked out, and how
to extend it. The official configuration software runs in a Windows VM, and
the mouse is passed through to the VM. The host records the USB traffic
between the two with `usbmon`.

## Requirements

- Linux with KVM, Docker with Compose, Wireshark (`tshark`, `dumpcap`)
- about 25 GB free disk space for the VM
- a second pointing device (e.g. a touchpad): while the VM runs, the mouse belongs to the VM

## Host setup

`dumpcap` drops its extra privileges before it opens `/dev/usbmon*`, so the
usbmon nodes themselves must be readable by the `wireshark` group:

```sh
sudo usermod -aG wireshark "$USER"     # takes effect after a reboot / new login
echo 'SUBSYSTEM=="usbmon", GROUP="wireshark", MODE="0640"' |
    sudo tee /etc/udev/rules.d/50-usbmon.rules
echo usbmon | sudo tee /etc/modules-load.d/usbmon.conf   # load usbmon at boot
sudo udevadm control --reload
sudo modprobe usbmon && sudo udevadm trigger --subsystem-match=usbmon
```

## Windows VM

[`tools/re/compose.yaml`](../tools/re/compose.yaml) runs
[dockur/windows](https://github.com/dockur/windows) with the wireless dongle
passed through. `KONECTL_RE_USB` selects the USB device for the VM: empty for
none, `-device usb-host,vendorid=0x1e7d,productid=0x2c92` for the cable.

```sh
tools/re/fetch-iso.sh                                          # Windows 11 LTSC evaluation ISO, 4.8 GB
KONECTL_RE_USB= docker compose -f tools/re/compose.yaml up -d  # install without passthrough
```

`fetch-iso.sh` downloads the ISO from Microsoft and checks its checksum. Run it
again to resume an interrupted download. The first start of the VM installs
Windows unattended, which takes about 20–30 minutes; watch it at
<http://127.0.0.1:8006>. With `KONECTL_RE_USB` empty, the mouse stays on
the host while Windows installs. Once Windows is up, recreate the container to
hand the mouse to the VM:

```sh
docker compose -f tools/re/compose.yaml up -d --force-recreate
```

Then
install the configuration software from Turtle Beach's support site inside the
VM. Swarm II does **not** support the Kone Pro Air (it is missing from its list of
supported products); the mouse needs the legacy **ROCCAT Swarm** (version
1.9482.0.0 was used here), linked from the Kone Pro Air's support page.

On first start Swarm asks for the Kone Pro Air module. Open its *Update
Center* and install the offered Swarm software update; after the reinstall
the mouse shows up (no firmware was flashed here). Over the dongle, Swarm's
title bar shows the dongle's firmware (4.19 here); connect the cable to see the
mouse's own firmware (4.32).

**Decline firmware updates.** An interrupted update over USB passthrough can
brick the mouse, and new firmware may change the protocol.

### Driving the VM from the host

While the mouse belongs to the VM, [`tools/re/vm.sh`](../tools/re/vm.sh) takes
screenshots and sends mouse clicks and key presses through QEMU (the QMP socket
that `compose.yaml` configures), so Swarm can be operated without a second
mouse:

```sh
tools/re/vm.sh shot                 # → tools/re/captures/screen.png
tools/re/vm.sh click 1148 737       # e.g. Swarm's Apply button (maximized window)
tools/re/vm.sh key meta_l-up        # maximize the focused window
tools/re/vm.sh type "roccat swarm"
```

Coordinates are pixels of the 1280×800 VM screen. Keep Swarm maximized so the
coordinates stay the same between sessions. Menus open at different heights
for different buttons, so take a screenshot after opening one.

The wireless mouse falls asleep after a few idle minutes, and Swarm then blocks
its window until the mouse is moved. For long sessions use the cable
(`KONECTL_RE_USB` as above) and unplug the dongle, so that `capture.sh` picks
the cable.

To use the mouse on the host in between without restarting Windows, detach it
from the VM through QMP and attach it again later; Swarm reads it again when it
comes back:

```sh
qmp() {
    printf '%s\n' '{"execute":"qmp_capabilities"}' "$1" |
        docker exec -i konectl-re-windows sh -c 'nc -U -q 1 /run/shm/qmp.sock'
}
qmp '{"execute":"qom-list","arguments":{"path":"/machine/peripheral-anon"}}'  # find usb-host
qmp '{"execute":"device_del","arguments":{"id":"/machine/peripheral-anon/device[3]"}}'
qmp '{"execute":"device_add","arguments":{"driver":"usb-host","vendorid":7805,"productid":11410,"id":"mouse"}}'
```

(`7805` and `11410` are `0x1e7d` and `0x2c92`, the cable; once attached this
way, the device is detached with `"id":"mouse"`.)

## Capturing

One capture per action, one change per capture:

```sh
tools/re/capture.sh dpi-stage1-850   # start, then change exactly one setting and apply it
# Ctrl+C after a few seconds of idle
tools/re/extract.sh tools/re/captures/01-dpi-stage1-850.pcapng
```

For scripted sessions, run the capture in the background and stop it when the
change is applied; `--stop` prints the extract:

```sh
tools/re/capture.sh --background dpi-stage1-850
tools/re/vm.sh click …               # change the setting, then Apply
tools/re/capture.sh --stop
```

`capture.sh` writes `tools/re/captures/<NN>-<name>.pcapng`, plus a `.dev` file
that records the bus, the device address and the config interface's endpoints.
`extract.sh` prints only the config-channel reports:

```
  1.234567  OUT  90 0a
  1.236012  IN   90 0a 04 00 00 32 00 00 e4 0a 36
```

Comparing the extracts of two captures that differ in one setting shows which
bytes encode it. Some writes carry data past their declared length (e.g. the
Easy-Shift block); `extract.sh --full` prints all 64 bytes of every report.

## Captures

The captures behind [protocol.md](protocol.md), one setting changed per capture
and applied with *Apply* (Swarm rewrites all profiles on every *Apply*):

| #     | Action                                       | Revealed                               |
|-------|----------------------------------------------|----------------------------------------|
| 00    | start Swarm with the mouse connected         | read commands, blocks, profile selection |
| 01–03 | DPI stage 2: 600 → 650 → 900 → 12800         | DPI unit (50), stage offset, `u16` LE  |
| 04–06 | stage 1: enable, set 400, disable            | disabled stage = `0`, no enable mask   |
| 07    | make stage 1 the active stage                | active-stage byte                      |
| 08–10 | polling rate 1000 → 500 → 125 → 250 Hz       | rate values                            |
| 11    | lift-off low → very low                      | lift-off byte                          |
| 12    | angle snapping on                            | angle-snapping byte                    |
| 13    | debounce 2 → 3 ms                            | debounce byte (Swarm writes every profile) |
| 14–16 | fully lit; left colour red; right colour `#123456` | effect byte, colour bytes (RGB order) |
| 17    | brightness                                   | brightness byte                        |
| 18–21 | wave; speed slider middle, minimum, maximum  | effect id, speed byte and range        |
| 22    | switch to profile slot 2                     | active-profile command                 |
| 23    | quit Swarm from the tray and start it again  | read-back of the written values        |
| 24–29 | effects off, heartbeat, breathing, blinking, battery, AIMO | effect ids               |
| 30    | start Swarm (second session)                 | same reads as 00                       |
| 31    | LED time-out 15 → 5 minutes                  | block 0 byte 17                        |
| 32–37 | sleep effect none, AIMO, heartbeat, fully lit, blinking, breathing | block 0 byte 18 (`ff` = never) |
| 38–39 | energy saving on, off                        | `10 72` with `aa` / `00`               |
| 40–41 | DPI "AUTO" on stage 2, off again             | block 0 byte 15 (bit per stage)        |
| 42–43 | colour intensity left 50, right 30 (stored 49, 29) | block 1 bytes 1–2                |
| 44    | lift-off "custom": calibration, then *Apply* | `10 08`, `90 08`, `10 15`, lift-off `08` |
| 45–47 | konectl on the host, repeated changes        | requests dropped after state changes   |
| 48    | start Swarm (third session)                  | blocks 2–4 of every profile            |
| 49    | mouse asleep while Swarm runs                | `90 09` answers `dd` instead of `cc`   |
| 50    | Swarm over the cable, idle                   | battery answer while charging          |
| 51–56 | six batches of button assignments, up to 11 buttons per *Apply* | slot order, block 2 codes, block 3/4 entries |
| 58    | profile colour red for slot 2                | profile table entry bytes 2–4          |
| 59–60 | "Open Swarm" on button 7, then pressed twice | `0b` entries are reported with `90 07` |

Closing Swarm's window only hides it in the tray, so it reads nothing when it
is opened again; quit it from the tray icon instead.

## Rules

- **Captures stay local.** `tools/re/captures/` is git-ignored. Before pasting
  an extract into the docs or a test, check it for serial numbers or other
  identifiers.
- **Only send what was captured.** Replaying an exact captured write with
  `konectl raw` is fine. Never guess command bytes: unknown commands can
  corrupt the mouse's settings.
- The configuration software in the VM is the recovery path if a write goes wrong.
