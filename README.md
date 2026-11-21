# konectl

Unofficial Linux tool and Rust library to configure the **ROCCAT Kone Pro Air** mouse.
Not affiliated with ROCCAT or Turtle Beach.

ROCCAT's configuration software (Swarm) only runs on Windows, and existing Linux
tools (libratbag/Piper, roccat-tools) don't support this mouse. konectl talks
to the mouse directly over hidraw.

## Status

| Feature                          | State                    |
|----------------------------------|--------------------------|
| Battery level                    | ✅                        |
| Wireless signal                  | ✅                        |
| Live LED colours (lost on sleep) | ✅                        |
| DPI, polling rate, saved lighting | 🚧 protocol not known yet |

See [docs/protocol.md](docs/protocol.md) for what is known about the protocol.

## Layout

- `crates/konectl`: the library (`Manager`, `KoneProAir`, `protocol`)
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
konectl led ff0000 0000ff       # left red, right blue
konectl led ff8800 -b 30        # both orange at 30 % brightness
konectl raw 90 0a               # send a raw report, print the replies (debugging)
```

`raw` is a tool for protocol work. Unknown commands can change or break the mouse
configuration, so only send what the official software was seen sending.

## Library

```rust
use konectl::{Manager, Rgb};

let manager = Manager::new()?;
let mut mouse = manager.open_first()?;
println!("battery: {:?}%", mouse.battery()?.percent());
mouse.set_leds(Rgb::new(255, 0, 0), Rgb::new(0, 0, 255))?;
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
