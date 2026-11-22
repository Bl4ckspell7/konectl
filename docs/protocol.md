# Kone Pro Air protocol notes

What is known about the configuration protocol of the ROCCAT Kone Pro Air, and
how konectl uses it. Fields marked *unknown* still need to be worked out.

The starting point was the [Eruption](https://github.com/eruption-project/eruption)
project's Kone Pro Air driver, which documents battery, signal and live LED
packets. konectl does not reuse Eruption's code. The settings protocol was
captured from ROCCAT Swarm 1.9482 talking to a mouse with firmware 4.32 (dongle
firmware 4.19); see
[reverse-engineering.md](reverse-engineering.md) for the method.

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

All byte offsets below are into the 64-byte report. Bytes that are not listed are `00`.

## Report format

| Offset | Request                                   | Data answer           |
|--------|-------------------------------------------|-----------------------|
| 0      | `90` = read, `10` = write                 | echo                  |
| 1      | command                                   | echo                  |
| 2      | payload length                            | payload length        |
| 3      | block number for `10 50`, otherwise `00`  | `00`                  |
| 4…     | payload                                   | payload               |

Short answers of the form `<op> <cmd> 03 00 ec <status> [code]` look like
status replies:

- `ec ac`: the write was accepted. Every write Swarm sends is answered this way.
- `ec fa <code>`: an error. It is seen for `90 00` without its payload (code `03`),
  for `90 70` over the cable (code `02`, no radio link), and for `90 0a` and
  `90 70` while the mouse sleeps (code `0d`).

While the mouse sleeps (or is switched off) the dongle answers on its own: the
info request gets `90 00 39 00` followed by zeros, battery and signal get the
error `0d`. Moving the mouse wakes it up.

## Commands

| Request                     | Meaning                                       |
|-----------------------------|-----------------------------------------------|
| `90 00 04 00 18 25 51 32`   | device info + settings block 0 of the selected profile |
| `90 01`, `90 02`, `90 04`, `90 05` | settings blocks 1, 2, 3, 4 of the selected profile |
| `10 50 <len> <block> …`     | write a settings block                        |
| `90 11` / `10 11`           | profile table                                 |
| `10 13 01 00 <p>`           | select the profile that reads return          |
| `10 03 01 00 <p>`           | set the active profile                        |
| `90 0a`                     | battery                                       |
| `90 70`                     | signal                                        |
| `10 10 0b 00 …`             | live LED colours (not saved)                  |
| `90 72` / `10 72 01 00 <v>` | energy saving: `aa` = on, `00` = off (one setting for all profiles) |
| `10 08 01 00 01`, `90 08`, `10 15 01 00 01` | lift-off calibration, see below |
| `90 09`, `90 71`, `10 0a`, `10 71` | *unknown*, see below |

### Device info: `90 00 04 00 18 25 51 32`

Swarm always sends the payload `18 25 51 32`; its meaning is *unknown*. Without
it (`90 00` alone) the answer is the error `90 00 03 00 ec fa 03`. konectl sends
the bare `90 00` once after opening the device and waits for that answer, so it
cannot be mistaken for the reply to the next request.

Answer, payload length `0x26`:

| Offset | Meaning                                                    |
|--------|------------------------------------------------------------|
| 4      | *unknown* (`00`)                                           |
| 5–6    | mouse firmware, major and minor: `04 20` = 4.32 (what Swarm shows over the cable) |
| 7      | *unknown* (`00`)                                           |
| 8–9    | `92 2c` = `0x2c92`, the cable's product ID (little-endian) |
| 10–28  | settings block 0, bytes 0–18 (see below)                   |
| 29     | *unknown* (`00`)                                           |
| 30–31  | dongle firmware, major and minor: `04 13` = 4.19 (what Swarm shows over the dongle); `00 00` over the cable |
| 32–40  | *unknown* (`00 00 01 04 00 00 01 02 00`; over the cable `00 00 01 04 00 00 00 00 00`) |
| 41     | debounce time in ms (block 0 byte 19)                      |

### Profiles

The mouse has five profile slots; Swarm uses slots 1–3.

- `10 13 01 00 <p>` selects profile `p` for the reads that follow (`90 00` and
  `90 01`…`90 05`). Swarm selects each profile in turn when it starts, and
  selects the active one again at the end.
- `10 03 01 00 <p>` makes profile `p` the active one. Swarm sends it on *Apply*
  when another profile slot was chosen.
- `90 11` returns the profile table: 5 entries of 5 bytes. Slots 1–3 hold
  `01 64 00 00 00`, slots 4 and 5 are zero. The meaning of the bytes is *unknown*.
- `10 11 06 00 <p> <5 bytes>` writes entry `p` of the table. Swarm sends it
  before the block writes of each profile, with the bytes it read.

### Settings blocks

| Block | Read                  | Write                 | Length | Content                    |
|-------|-----------------------|-----------------------|--------|----------------------------|
| 0     | `90 00 04 00 18 25 51 32` (offset 10) | `10 50 14 00 …` | 20 | DPI, polling rate, lift-off, angle snapping, debounce |
| 1     | `90 01` (offset 4)    | `10 50 0b 01 …`       | 11     | saved lighting             |
| 2     | `90 02` (offset 4)    | `10 50 1c 02 …`       | 28     | *unknown*                  |
| 3     | `90 04` (offset 4)    | `10 50 18 03 …`       | 24     | button assignment (layout *unknown*) |
| 4     | `90 05` (offset 4)    | `10 50 18 04 …`       | 24     | Easy-Shift assignment (layout *unknown*) |

Writes take effect immediately and are kept: Swarm reads them back after a
restart. There is no separate save command.

#### Block 0: sensor and DPI

Offsets are into the block (write: report offset 4 + n; read: offset 10 + n).

| Byte  | Meaning                                                        |
|-------|----------------------------------------------------------------|
| 0     | profile number, 1–5                                            |
| 1     | polling rate: `00` = 1000 Hz, `01` = 500, `02` = 250, `03` = 125 |
| 2     | angle snapping: `00` = off, `01` = on                          |
| 3     | active DPI stage, 0-based                                      |
| 4–13  | DPI stages 1–5, little-endian `u16` each, in steps of 50 DPI (`0x0012` = 900). `0` = stage disabled |
| 14    | lift-off distance: `06` = very low, `07` = low, `08` = custom (calibrated) |
| 15    | DPI "AUTO" flags: bit 0 = stage 1 … bit 4 = stage 5 (Swarm offers it only for enabled stages) |
| 16    | *unknown* (`06` in slot 1, `00` in slots 2–3); reads have shown bit 7 set (`86`), Swarm writes it cleared |
| 17    | LED time-out in minutes (`0f` = 15; Swarm's list starts at 1) |
| 18    | effect after the LED time-out, codes as in [block 1](#block-1-saved-lighting): `00` off, `01` static, `02` breathing, `05` heartbeat, `08` blinking, `09` AIMO; `ff` (with byte 17 = `00`) = never time out |
| 19    | debounce time in ms (write only; the read returns it at offset 41) |

All of block 0 is stored per profile. Swarm shows only one debounce time
(0–10 ms) and writes it into every profile; the others are set per profile in
Swarm. `konectl debounce` does the same.

Swarm's slider goes from 50 to 19000 DPI. A disabled stage is stored as `0` and
shown as 50.

#### Block 1: saved lighting

| Byte | Meaning                                                    |
|------|------------------------------------------------------------|
| 0    | effect (table below)                                       |
| 1–2  | intensity of the left and right colour, 0–100 ("I" in Swarm's colour picker) |
| 3    | brightness, 0–100 (`0x64`)                                 |
| 4    | speed, 1–11 (`0x01`–`0x0b`)                                |
| 5–7  | left zone R, G, B                                          |
| 8–10 | right zone R, G, B                                         |

| Effect | Name in Swarm                    |
|--------|----------------------------------|
| `00`   | lighting off                     |
| `01`   | fully lit (static)               |
| `02`   | breathing                        |
| `05`   | heartbeat                        |
| `06`   | wave                             |
| `08`   | blinking                         |
| `09`   | AIMO (intelligent lighting)      |
| `0a`   | battery indication (Swarm sets brightness to 100) |

The colours are kept when switching effects. With effect `09`, `90 01` answers
with the colours that are currently shown, not the saved ones.

### Apply sequence (Swarm)

On *Apply*, Swarm rewrites everything, waiting for the `ec ac` answer after each write:

1. `10 11 06 00 04`, `10 11 06 00 05` (clears the unused slots 4 and 5)
2. `10 71 05 00 aa 03 00 aa aa`, `10 72 01`: the bytes it read with `90 71`
   and `90 72` at start-up
3. for each profile 1–3: `10 11` (table entry), then blocks 0, 1, 2, 3 and 4 (`10 50`)

Writing only the table entry and the changed block of one profile is enough.
Replaying `10 11 06 00 01 01 64`, a block 0 write and a block 1 write from
Linux changed the settings, and they read back correctly.

konectl changes settings the same way: it reads blocks 0 and 1 of the
profile, changes only the fields it knows, writes the table entry (as read)
and both blocks, and reads them back to check them.

The mouse sometimes drops a request that arrives a few milliseconds after a
state change (a profile selection or a block write): it never answers it.
konectl therefore pauses 40 ms after every write (Swarm leaves 60–90 ms between
commands) and sends an unanswered request again, up to two more times. That is
safe because every request reads, or sets an absolute value.

### Lift-off calibration

Swarm's "custom" lift-off distance is measured on the user's surface:

1. `10 08 01 00 01` starts the measurement. The mouse does not answer it; Swarm
   reads `90 08` one second later (answer `90 08 01 00 00`).
2. The user moves the mouse continuously; Swarm counts 10 seconds itself and
   polls nothing else.
3. `10 15 01 00 01` (answered `ec ac`) keeps the measured distance.
4. Swarm then writes block 0 of every profile with lift-off `08` (custom).

Cancelling the dialog before the start sends nothing, but Swarm then writes
lift-off "low" (`07`) into every profile.

Swarm quirks seen in the captures:

- Each *Apply* stores the brightness one lower than it read (70 → 69); it
  converts it through the slider position. Colour intensities typed into the
  picker are stored one lower too (50 → 49).
- Enabling a stage without changing its value (still shown as 50) sends nothing.

### Battery: `90 0a`

| Answer offset | Meaning                     |
|---------------|-----------------------------|
| 0–1           | `90 0a` (echo)              |
| 10            | battery level, percent (values > 100 *unknown*; konectl reports them as unknown) |
| 4, 6          | `01` on cable, `00` on battery; charging flag? |
| 8–9           | little-endian; 2788–2789 on battery, 2866 charging; voltage-related? |
| 2, 5          | `04` (payload length) and *unknown* (`32` so far) |

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
| 4             | raw signal value                                |

The unit of the raw value is *unknown*. konectl estimates quality as
`clamp(128 - raw, 0, 100)` %.

Observed answers:

- dongle, mouse ~1 m away: `90 70 01 00 37 00 …`; the raw value varies between reads (53–59).
- cable: `90 70 03 00 ec fa 02 00 …`, the error status reply (no radio link).
  konectl treats `0xec` at offset 4 as "radio off".

### Live LED colours: `10 10 0b 00 09 64 64 64 06`

The payload has the layout of [block 1](#block-1-saved-lighting): effect `09`,
`64 64`, brightness 100, speed 6, then the colours.

| Offset | Meaning                                   |
|--------|-------------------------------------------|
| 4–8    | effect, `64 64`, brightness, speed        |
| 9–11   | left zone R, G, B                         |
| 12–14  | right zone R, G, B                        |

- Volatile: the colours stay while the mouse is in use, but once it goes to sleep
  it wakes with its stored effect again (e.g. rainbow). Nothing is saved.
- Answer: `10 10 03 00 ec ac 00 …` (accepted).
- Brightness is applied by scaling the RGB values in software.

### Not understood yet

- `90 09`: answer `90 09 01 00 cc`. Swarm polls it every few seconds, together with `90 70`.
- `10 0a 01 00 <b>`: Swarm sends `01` once when it starts, and `00` after
  reading the battery.
- `90 71` (answer `aa 03 00 aa aa`): written back unchanged by every *Apply*.
- Blocks 2–4, the bytes of the profile table, block 0 byte 16.

Swarm features that are not stored on the mouse: the DPI *Calibrate* dialog
(an aiming game that suggests a DPI value) and the colour picker's *Gradient*
mode (not selectable for this mouse).
