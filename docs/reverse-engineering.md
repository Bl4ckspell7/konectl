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
passed through (edit `productid` to `0x2c92` for the cable).

```sh
tools/re/fetch-iso.sh                                         # Windows 11 LTSC evaluation ISO, 4.8 GB
KONECTL_RE_ARGUMENTS= docker compose -f tools/re/compose.yaml up -d   # install without passthrough
```

`fetch-iso.sh` downloads the ISO from Microsoft and checks its checksum. Run it
again to resume an interrupted download. The first start of the VM installs
Windows unattended, which takes about 20–30 minutes; watch it at
<http://127.0.0.1:8006>. With `KONECTL_RE_ARGUMENTS` empty, the mouse stays on
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
the mouse shows up (no firmware was flashed here, firmware 4.19).

**Decline firmware updates.** An interrupted update over USB passthrough can
brick the mouse, and new firmware may change the protocol.

## Capturing

One capture per action, one change per capture:

```sh
tools/re/capture.sh dpi-stage1-850   # start, then change exactly one setting and apply it
# Ctrl+C after a few seconds of idle
tools/re/extract.sh tools/re/captures/01-dpi-stage1-850.pcapng
```

`capture.sh` writes `tools/re/captures/<NN>-<name>.pcapng`, plus a `.dev` file
that records the bus, the device address and the config interface's endpoints.
`extract.sh` prints only the config-channel reports:

```
  1.234567  OUT  90 0a
  1.236012  IN   90 0a 04 00 00 32 00 00 e4 0a 36
```

Comparing the extracts of two captures that differ in one setting shows which
bytes encode it.

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
| 13    | debounce 2 → 3 ms                            | debounce byte (global)                 |
| 14–16 | fully lit; left colour red; right colour `#123456` | effect byte, colour bytes (RGB order) |
| 17    | brightness                                   | brightness byte                        |
| 18–21 | wave; speed slider middle, minimum, maximum  | effect id, speed byte and range        |
| 22    | switch to profile slot 2                     | active-profile command                 |
| 23    | quit Swarm from the tray and start it again  | read-back of the written values        |
| 24–29 | effects off, heartbeat, breathing, blinking, battery, AIMO | effect ids               |

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
