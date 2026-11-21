# Kone Pro Air protocol notes

What is known about the configuration protocol of the ROCCAT Kone Pro Air, and
how konectl uses it. Fields marked *unknown* still need to be worked out.

The starting point was the [Eruption](https://github.com/eruption-project/eruption)
project's Kone Pro Air driver, which documents battery, signal and live LED
packets. konectl does not reuse Eruption's code.

## USB identities

| Mode            | VID:PID     | Vendor channel interface |
|-----------------|-------------|--------------------------|
| Wireless dongle | `1e7d:2c8e` | 2                        |
| USB cable       | `1e7d:2c92` | 1                        |

Other interfaces: 0 = mouse input, 3 = consumer control / system control, and
the keyboard (button macros) on 1 (dongle) or 2 (cable). Dongle and cable swap
interfaces 1 and 2.

## Vendor channel

- HID usage page `0xFF00` (dongle) or `0xFF13` (cable), usage `0x01`.
- 64-byte input and output reports, **no report IDs**.
- Writing via hidraw or hidapi: prefix the 64-byte payload with a `0x00` report-ID byte (65 bytes in total).
- Reading: 64 bytes, no prefix.
- Answers come back as input reports on the same interface. Other reports can
  arrive in between, so readers must match the answer by its header and skip the rest.

All byte offsets below are into the 64-byte payload. Bytes that are not listed are `00`.

## Commands

### Init: `90 00`

Sent once after opening the device. Answer: `90 00 03 00 ec fa 03 00 …`
(content not understood). konectl waits for it, so it cannot be mistaken for
the reply to the next request.

### Battery: `90 0a`

| Answer offset | Meaning                     |
|---------------|-----------------------------|
| 0–1           | `90 0a` (echo)              |
| 10            | battery level, percent (values > 100 *unknown*; konectl reports them as unknown) |
| 4, 6          | `01` on cable, `00` on battery; charging flag? |
| 8–9           | little-endian; 2788–2789 on battery, 2866 charging; voltage-related? |
| 2, 5          | *unknown* (always `04`, `32` so far) |

Observed answers:

| Situation           | Answer                                    |
|---------------------|-------------------------------------------|
| dongle, 54 %        | `90 0a 04 00 00 32 00 00 e4 0a 36 00 …`   |
| dongle, 50 %        | `90 0a 04 00 00 32 00 00 e5 0a 32 00 …`   |
| cable (charging), 50 % | `90 0a 04 00 01 32 01 00 32 0b 32 00 …` |

### Signal: `90 70`

| Answer offset | Meaning                                         |
|---------------|-------------------------------------------------|
| 0–1           | `90 70` (echo)                                  |
| 4             | raw signal value; `236` (`0xec`) = radio off    |

The unit of the raw value is *unknown*. konectl estimates quality as
`clamp(128 - raw, 0, 100)` %.

Observed answers:

- dongle, mouse ~1 m away: `90 70 01 00 37 00 …`; the raw value varies between reads (55–59).
- cable: `90 70 03 00 ec fa 02 00 …`, raw `0xec` = radio off.

### Live LED colours: `10 10 0b 00 09 64 64 64 06`

| Offset | Meaning                                   |
|--------|-------------------------------------------|
| 0–8    | fixed header; meaning of bytes 1–8 *unknown* (`0x64` = 100?) |
| 9–11   | left zone R, G, B                         |
| 12–14  | right zone R, G, B                        |

- Volatile: the colours stay while the mouse is in use, but once it goes to sleep
  it wakes with its stored effect again (e.g. rainbow). Nothing is saved.
- Answer: `10 10 03 00 ec ac 00 …`; bytes 2–5 *unknown* (checksum?).
- Brightness is applied by scaling the RGB values in software.

## Unknown (phase 2)

To be captured from the official software (Swarm II):

- DPI stages (set, read, active stage)
- polling rate, lift-off distance
- profiles, button mapping
- stored LED effects (static, wave, breathing, …) and brightness
- firmware version
- save/commit command
