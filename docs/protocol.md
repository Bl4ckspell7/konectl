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
| `10 62 08 00 36 31 18 38 27 98 10 94` | factory reset, see [below](#factory-reset) |
| `10 09 01 00 01` to the dongle | start pairing, see [below](#pairing)     |
| `10 <1f+N> 3c <chunk> …`, `10 60 06 00 92 2c 00 00 00 <N>` | macro `N`: record and commit, see [macros](#macros) |
| `90 09` to the dongle       | connection state: `cc` = mouse connected and awake, `dd` = asleep (Swarm polls it) |
| `90 07 …` from the mouse    | button press for a function Swarm carries out, see [buttons](#blocks-24-buttons) |
| `90 71`, `10 0a`, `10 71`   | *unknown*, see below |

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

The mouse has five profile slots. Swarm uses the first ones, as many as there
are profiles (three when it is installed).

- `10 13 01 00 <p>` selects profile `p` for the reads that follow (`90 00` and
  `90 01`…`90 05`). Swarm selects each profile in turn when it starts, and
  selects the active one again at the end.
- `10 03 01 00 <p>` makes profile `p` the active one. Swarm sends it on *Apply*
  when another profile slot was chosen.
- `90 11` returns the profile table: 5 entries of 5 bytes. Slots 1–3 hold
  `01 64 00 00 00`, slots 4 and 5 are zero. Byte 0 is `01` for a slot in use,
  byte 1 is *unknown* (`64`), bytes 2–4 are the profile colour (RGB) that the
  DPI LED flashes when the mouse switches to the profile; `00 00 00` = none
  (Swarm's red is `f4 00 00`).
- `10 11 06 00 <p> <5 bytes>` writes entry `p` of the table. Swarm sends it
  before the block writes of each profile, with the bytes it read, and a zero
  entry for every unused slot.
- Adding a profile (*+*): Swarm writes the entry of the next slot and all five
  blocks with its own defaults, which differ per slot (slot 4: heartbeat
  lighting, block 0 byte 16 = `80`; slot 5: static blue, byte 16 = `01`; both
  with DPI 400–3200, buttons 6/7 forward/back, button 8 profile cycle, no
  Easy-Shift button, debounce 10 ms).
- Deleting one (*Delete Profile*): the profiles after it move down one slot
  (entry and blocks rewritten with the new number), and the entry of the
  last slot is written as zeros.

konectl adds a profile as a copy of an existing one (without its colour) and
removes one the same way as Swarm; it does not remove the active profile.
Buttons that switch to a profile (`48`–`4a`) are renumbered to keep pointing
at the same profile, and disabled if they pointed at the removed one.
Swarm's profile menu also offers export, import, linked applications,
*Set as Desktop Profile* and *Profile Autoswitch*, which exist in Swarm only.

### Settings blocks

| Block | Read                  | Write                 | Length | Content                    |
|-------|-----------------------|-----------------------|--------|----------------------------|
| 0     | `90 00 04 00 18 25 51 32` (offset 10) | `10 50 14 00 …` | 20 | DPI, polling rate, lift-off, angle snapping, debounce |
| 1     | `90 01` (offset 4)    | `10 50 0b 01 …`       | 11     | saved lighting             |
| 2     | `90 02` (offset 4)    | `10 50 1c 02 …`       | 28     | button function codes      |
| 3     | `90 04` (offset 4)    | `10 50 18 03 …`       | 24     | button entries, standard layer |
| 4     | `90 05` (offset 4)    | `10 50 18 04 …`       | 24     | button entries, Easy-Shift layer |

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

#### Blocks 2–4: buttons

Every button has a function in two layers: the standard layer, and the
Easy-Shift[+] layer, which applies while a button with the Easy-Shift function
is held. Swarm numbers the buttons 1–8 (1 left, 2 right, 3 wheel click, 4 and 5
wheel up and down, 6 and 7 the front and rear side button, 8 the profile
button); the blocks store them in another order:

| Slot   | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 |
|--------|---|---|---|---|---|---|---|---|
| Button | 1 | 2 | 3 | 7 | 6 | 8 | 4 | 5 |

- Block 2, bytes 0–7: the function code of slots 1–8 in the standard layer;
  bytes 8–15: the same for the Easy-Shift layer; bytes 16–27: how macros 1–12
  are played (see [macros](#macros)).
- Blocks 3 (standard layer) and 4 (Easy-Shift layer): a three-byte entry per
  slot. It is used with function code `40`; with any other code Swarm writes
  `00 00 00`.

| Code (block 2)    | Function                                              |
|-------------------|-------------------------------------------------------|
| `00`              | disabled                                              |
| `01`–`0c`         | play macro 1–12 (entry `00 00 00`), see [macros](#macros) |
| `40`              | what the entry says (next table)                      |
| `41`              | profile cycle                                         |
| `42`              | DPI cycle                                             |
| `45`              | Easy-Shift[+] (also "Easy-Shift[+] All Devices")      |
| `46`, `47`        | next profile, previous profile                        |
| `48`–`4a`         | switch to profile 1–3 ("Profile presets")             |
| `4e`, `4f`, `50`, `51` | Easy-Wheel (hold and turn the wheel): DPI, volume, Alt-Tab, Aero Flip 3D |
| `52`, `53`        | DPI up, DPI down                                      |
| `54`, `56`        | Easy-Aim presets: 200 and 900 DPI while the button is held |

| Entry (blocks 3, 4) | Function                                            |
|---------------------|-----------------------------------------------------|
| `00 NN 00`          | mouse function: `01` click, `02` menu (right click), `03` universal scrolling (middle click), `04` browser back, `05` browser forward, `06` double click, `07` scroll up, `08` scroll down |
| `01 MM KK`          | key: HID keyboard usage `KK` with the HID modifier mask `MM` (`01` Ctrl, `02` Shift, `04` Alt, `08` Win); `KK` = `00` for modifiers alone ("Left ctrl" is `01 01 00`) |
| `02 LL HH`          | consumer control, little-endian HID usage: `e2` mute, `e9`/`ea` volume up/down, `b5`/`b6` next/previous track, `b7` stop, `cd` play/pause, `0221` browser search, `0226` browser stop, `0227` browser refresh |
| `0c LL HH`          | Easy-Aim with a custom DPI, little-endian in steps of 50 (`2c 01` = 15000) |
| `05 NN 00`          | system control, sent by the mouse itself: `74` power down, `8e` sleep, `8f` wake up (see below) |
| `0b NN 00`          | carried out by Swarm: `02` open Swarm, `04` stop timer, `07` open website, `23` hibernate, `24` reboot, `25` lock, `26` log off, `29` task manager |

Swarm's "Browser backwards" and "Browser forwards" are the mouse functions `04`
and `05`; "New tab" and "New window" are the keys Ctrl+T and Ctrl+N; hotkeys
are key entries.

When a button with a `0b` entry is pressed, the mouse sends
`90 07 04 00 <code> <entry> <slot>` on the vendor channel (`90 07 04 00 40 0b
02 04` for "Open Swarm" on slot 4) and Swarm carries out the function. The
website, file or program to open is stored in Swarm only.

The `05` entries work without Swarm: the mouse sends them as HID Generic
Desktop System Control on its interface 3 (report ID 1, one bit each for
System Power Down `0x81`, Sleep `0x82` and Wake Up `0x83`). With Swarm closed,
pressing the buttons sent `01 01 00`, `01 02 00` and `01 04 00`, each followed by
`01 00 00` on release, and *power down* shut Windows down.

What Swarm offers (konectl follows it):

- Button 8 takes only the DPI and profile functions (not the presets) and has
  no Easy-Shift function.
- The wheel (buttons 4 and 5) takes neither macros nor the ROCCAT functions
  (Easy-Shift, Easy-Aim, Easy-Wheel).
- The Easy-Shift layer has no Easy-Shift function. Making a button an
  Easy-Shift button disables its Easy-Shift function (code `00`); giving
  button 6 another function makes its Easy-Shift function available.
- *RESET* gives a button its default function (button 7: browser back).
- Left and right click must stay on a button: to change button 2, another
  button needs *Menu* first.

konectl also keeps button 1 as click in the standard layer, so the mouse stays
usable.

#### Macros

The mouse plays macros by itself. Their numbers belong to the buttons that can
play one: slots 1–5 (buttons 1, 2, 3, 7, 6) are macros 1–5 in the standard
layer and macros 7–11 in the Easy-Shift layer. A button plays its macro when
its function code is that number.

The macros are **shared by all profiles**: there is one macro per number, and
every profile whose button plays it plays the one written last. (Writing
macro 4 while assigning it in profile 2 changed what button 7 typed in profile
1 too.) Block 2 bytes 16–27 hold how macros 1–12 are played, per profile:
`01`–`63` = once per press, repeated that many times (Swarm offers 1–99),
`fd` = while the button is held, `fe` = start and stop with a press ("Macro
Toggle"), `00` = not assigned.

A macro is written as a record of 60-byte chunks, `10 <1f+N> 3c <chunk>`
(macro 3: `10 22`, macro 10: `10 29`), followed by
`10 60 06 00 92 2c 00 00 00 <N>` (answered after about 200 ms). Swarm sends
them after the blocks of the profile that assigns the macro. The record:

| Offset | Meaning                                                      |
|--------|--------------------------------------------------------------|
| 0      | macro number                                                 |
| 1–3    | `09 04 38` in every record Swarm wrote                       |
| 4–31   | macro group name in Swarm, ASCII, zero-padded                |
| 32–59  | macro name                                                   |
| 60     | number of events                                             |
| 61–62  | length of the events in bytes (5 per event), little-endian   |
| 63–64  | `00 00`                                                      |
| 65…    | events, then zeros up to the end of the last chunk           |

An event is `<01 = press, 81 = release> <modifier mask> <HID usage> <delay,
little-endian ms>`, with the delay after it. Swarm writes a modifier key as an
event with its bit in the mask and usage `00` (Shift down: `01 02 00 …`), and
the last event with delay 0. Swarm records only keyboard keys.

Nothing reads a macro back: Swarm keeps its macros on the computer and only
writes them. Changing a button from a macro to another function only changes
block 2 (code and mode `00`); the record stays. Keys are HID usages (named
after the US layout); with a German layout the computer types `z` for `y`.

### Apply sequence (Swarm)

On *Apply*, Swarm rewrites everything, waiting for the `ec ac` answer after each write:

1. `10 11 06 00 04`, `10 11 06 00 05` (clears the unused slots 4 and 5)
2. `10 71 05 00 aa 03 00 aa aa`, `10 72 01`: the bytes it read with `90 71`
   and `90 72` at start-up
3. for each profile 1–3: `10 11` (table entry), then blocks 0, 1, 2, 3 and 4 (`10 50`)

Block writes go to the profile named in byte 0 of the last block 0 the mouse
received, not to the profile of the table entry or the one selected for reads:
writing only the table entry and blocks 2–4 of profile 2 changed profile 3,
whose block 0 Swarm had written last. Block 0 therefore comes first.

Writing the table entry, block 0 and the changed blocks of one profile is
enough. Replaying `10 11 06 00 01 01 64`, a block 0 write and a block 1 write
from Linux changed the settings, and they read back correctly.

konectl changes settings the same way: it reads the blocks of the profile,
changes only the fields it knows, writes the table entry, block 0 (changed, or
as read when only other blocks change) and the other changed blocks in Swarm's
order, and reads the written blocks back to check them.

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

### Factory reset

`10 62 08 00 36 31 18 38 27 98 10 94` resets the mouse (the same payload in two
captures; its meaning is *unknown*). The answer `ec ac` comes after about 1.5
seconds, and Swarm sends nothing else. Afterwards:

- the profile slots in use stay in use, with default settings: DPI 400, 800,
  1200, 1600 and 3200 (stage 2 active), lift-off very low, debounce 10 ms, LED
  time-out 15 minutes, a default lighting and colour per slot (slot 1: AIMO and
  purple `bf 0b dc`, slot 2: wave and white, slot 3: breathing and red, slot 4:
  heartbeat and green `51 ff 00`);
- blocks 2–4 read `ff` everywhere except button 8 (code `41`, profile cycle):
  the firmware's default functions;
- block 0 bytes 15 and 16 read `ff` at first;
- energy saving is on.

### Pairing

Swarm's *Pair* button sends `10 09 01 00 01` to the dongle (answered `ec ac`).
The mouse is then paired by hand, as Swarm's dialog says: unplug its cable,
hold the profile button for three seconds, and plug the dongle in again when
the lighting effect has finished. `90 09` on the dongle answers `cc` once the
mouse is connected. `konectl pair` sends the command, waits until the dongle
was plugged in again and checks that the mouse answers through it.

### Battery: `90 0a`

| Answer offset | Meaning                     |
|---------------|-----------------------------|
| 0–1           | `90 0a` (echo)              |
| 10            | battery level, percent (values > 100 *unknown*; konectl reports them as unknown) |
| 4             | `01` while the mouse is powered over the cable |
| 5             | battery level in steps of 10 % (`32` at 54 %, `5a` at 90 %) |
| 6             | `01` while the battery charges; `00` on battery and once it is full on the cable |
| 8–9           | little-endian; 2788–2789 on battery, 2866 charging; voltage-related? |
| 2             | `04` (payload length)       |

Observed answers:

| Situation           | Answer                                    |
|---------------------|-------------------------------------------|
| dongle, 54 %        | `90 0a 04 00 00 32 00 00 e4 0a 36 00 …`   |
| dongle, 50 %        | `90 0a 04 00 00 32 00 00 e5 0a 32 00 …`   |
| cable (charging), 50 % | `90 0a 04 00 01 32 01 00 32 0b 32 00 …` |
| cable (charging), 90 % | `90 0a 04 00 01 5a 01 00 fb 0b 5a 00 …` |
| cable, full, 100 %  | `90 0a 04 00 01 64 00 00 f3 0b 64 00 …` |

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
- Swarm draws the AIMO effect itself with this report: while it runs, it
  sends new colours about every 45 ms.

### Not understood yet

- `10 0a 01 00 <b>`: Swarm sends `01` once when it starts, and `00` after
  reading the battery.
- `90 71` (answer `aa 03 00 aa aa`): written back unchanged by every *Apply*.
- Block 0 byte 16, byte 1 of the profile table entries (`64`), function
  code `55` (between the two Easy-Aim presets), bytes 1–3 of a macro record,
  macro numbers 6 and 12 (no button uses them), Swarm's limit for the length
  of a macro (100 events in ten chunks worked).
- The payload of the reset command. Unused profile slots keep leftovers after
  a reset (slot 5: `00 64 f6 ff 39`); byte 0 `00` marks them unused.

Swarm features that are not stored on the mouse: the DPI *Calibrate* dialog
(an aiming game that suggests a DPI value), the colour picker's *Gradient*
mode (not selectable for this mouse), timers, the targets of *Open …*, the
button *Presets* list and the profile names.
