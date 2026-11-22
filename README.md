# konectl

Unofficial Linux tool and Rust library to configure the **ROCCAT Kone Pro Air** mouse.
Not affiliated with ROCCAT or Turtle Beach.

ROCCAT's configuration software (Swarm) only runs on Windows, and existing Linux
tools (libratbag/Piper, roccat-tools) don't support this mouse. konectl talks
to the mouse directly over hidraw.

## Status

| Feature                                   | State |
|-------------------------------------------|-------|
| Battery level, wireless signal            | ✅     |
| DPI stages and active stage               | ✅     |
| Polling rate, lift-off distance           | ✅     |
| Saved lighting (effect, colours, brightness, speed) | ✅ |
| Profiles (show, activate, per-profile settings) | ✅ |
| Backup and restore of these settings      | ✅     |
| Live LED colours (lost on sleep)          | ✅     |
| Button assignment, macros                 | ❌ not decoded |

Tested with firmware 4.19, over the wireless dongle and the USB cable. See
[docs/protocol.md](docs/protocol.md) for the protocol and
[docs/reverse-engineering.md](docs/reverse-engineering.md) for how it was worked out.

## Layout

- `crates/konectl`: the library (`Manager`, `KoneProAir`, `settings`, `protocol`)
- `tools/re`: Windows VM and USB capture scripts used to decode the protocol
- `crates/konectl-cli`: the `konectl` command-line tool

## Build

Requires Rust 1.88 or newer and libudev (`systemd-libs` on Arch).

```sh
cargo build --release
```

## Permissions

By default, only root can open the mouse's `/dev/hidraw*` nodes. Install the udev rule to
give the logged-in user access to the configuration interface only:

```sh
sudo install -m 0644 udev/70-konectl.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger
```

## Usage

```sh
konectl list                    # connected devices
konectl battery                 # e.g. 87%
konectl signal                  # e.g. 92% (raw 36)

konectl settings                # settings of the active profile
konectl settings -p 2           # … of profile 2
konectl dpi 2 900               # DPI stage 2 → 900 DPI
konectl dpi 3 off               # disable stage 3
konectl stage 2                 # make stage 2 the active stage
konectl rate 1000               # polling rate: 125, 250, 500 or 1000 Hz
konectl liftoff low             # lift-off distance: very-low or low
konectl lighting static --left ff0000 --right 0000ff -b 60
konectl lighting wave -s 4      # effects: off, static, breathing, heartbeat,
                                #          wave, blinking, aimo, battery
konectl profile                 # profiles in use; konectl profile 2 activates one

konectl backup my-mouse.txt     # save all profiles
konectl restore my-mouse.txt    # write them back

konectl led ff0000 0000ff       # live colours until the mouse sleeps (not saved)
konectl raw 90 0a               # send a raw report, print the replies (debugging)
```

The settings commands change the active profile unless `-p` is given. They are
saved on the mouse and checked by reading them back. The mouse must be awake:
move it if konectl reports that it does not answer.

Before each change konectl saves the current settings of all profiles to
`${XDG_STATE_HOME:-~/.local/state}/konectl/previous.txt`; the state before the
first change ever is kept in `original.txt`. Undo a change with
`konectl restore ~/.local/state/konectl/previous.txt`.

`raw` is a tool for protocol work. Unknown commands can change or break the mouse
configuration, so only send what the official software was seen sending.

## Library

```rust
use konectl::{Manager, Rgb};

let manager = Manager::new()?;
let mut mouse = manager.open_first()?;
println!("battery: {:?}%", mouse.battery()?.percent());

let mut settings = mouse.settings(None)?; // active profile
settings.sensor.set_dpi(2, Some(900))?;
settings.lighting.set_left(Rgb::new(255, 0, 0));
mouse.write_settings(&settings)?;
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
