//! Macros: key sequences the mouse plays by itself when a button is pressed.
//!
//! A macro is a list of key events (a key or a modifier pressed or released)
//! with the delay after each one. The mouse stores one macro per button and
//! layer that can take one (numbers 1–5 and 7–11), **shared by all
//! profiles**: a profile whose
//! button plays a macro plays the one last written for that button. The
//! button's function code in block 2 is the macro's number, and block 2 also
//! holds how it is played ([`MacroMode`](crate::buttons::MacroMode)); both are
//! per profile. The mouse cannot report a macro back. See `docs/protocol.md`.
//!
//! Keys are HID usages, named after the US layout; with another keyboard
//! layout the computer may type other characters (`y` and `z` are swapped on
//! a German one).

use std::fmt;
use std::str::FromStr;

use crate::buttons::{Key, MODIFIERS};
use crate::error::{Error, Result};

/// Most events konectl writes into one macro (100 events, ten chunks, were
/// tested on the mouse; the official software's own limit is not known).
pub const MACRO_EVENTS_MAX: usize = 100;
/// Delay after each event of a key token, in milliseconds, unless a delay
/// follows it in the macro text.
pub const DEFAULT_DELAY_MS: u16 = 20;

/// Length of one chunk of a macro record (`10 <cmd> 3c <chunk>`).
pub(crate) const MACRO_CHUNK_LEN: usize = 60;
/// Bytes 1–3 of every record the official software wrote.
const RECORD_MAGIC: [u8; 3] = [0x09, 0x04, 0x38];
/// Length of the group and of the macro name in a record.
const NAME_LEN: usize = 28;
const GROUP_NAME: &str = "konectl";
const EVENT_LEN: usize = 5;
const EVENT_PRESSED: u8 = 0x01;
const EVENT_RELEASED: u8 = 0x81;

/// One event of a macro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacroEvent {
    /// `true` for a key press, `false` for its release.
    pub pressed: bool,
    /// A single modifier bit (`01` Ctrl, `02` Shift, `04` Alt, `08` Super) for a
    /// modifier key, or `0`.
    pub modifiers: u8,
    /// HID keyboard usage of a key, or `0` for a modifier key.
    pub usage: u8,
    /// Wait after the event, in milliseconds.
    pub delay_ms: u16,
}

impl MacroEvent {
    fn check(self) -> Result<()> {
        let key = Key {
            modifiers: self.modifiers,
            usage: self.usage,
        };
        key.check()?;
        if self.modifiers.count_ones() + u32::from(self.usage != 0) != 1 {
            return Err(Error::InvalidValue(format!(
                "a macro event is a single key or modifier, not {key}"
            )));
        }
        Ok(())
    }

    fn bytes(self) -> [u8; EVENT_LEN] {
        let [low, high] = self.delay_ms.to_le_bytes();
        let kind: u8 = if self.pressed {
            EVENT_PRESSED
        } else {
            EVENT_RELEASED
        };
        [kind, self.modifiers, self.usage, low, high]
    }

    /// The key or modifier as konectl writes it, e.g. `ctrl` or `c`.
    fn key(self) -> Key {
        Key {
            modifiers: self.modifiers,
            usage: self.usage,
        }
    }
}

/// A key sequence for the mouse to play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Macro {
    events: Vec<MacroEvent>,
}

impl Macro {
    /// A macro of `events` (1 to [`MACRO_EVENTS_MAX`]), each a single key or
    /// modifier.
    pub fn new(events: Vec<MacroEvent>) -> Result<Self> {
        if events.is_empty() || events.len() > MACRO_EVENTS_MAX {
            return Err(Error::InvalidValue(format!(
                "a macro has 1 to {MACRO_EVENTS_MAX} events, not {}",
                events.len()
            )));
        }
        for event in &events {
            event.check()?;
        }
        Ok(Self { events })
    }

    pub fn events(&self) -> &[MacroEvent] {
        &self.events
    }

    /// The record the mouse stores as macro `number`, in chunks of
    /// [`MACRO_CHUNK_LEN`] bytes: number, `09 04 38`, group and macro name
    /// (28 bytes each), event count, length of the events (LE16), `00 00`,
    /// then five bytes per event.
    pub(crate) fn record(&self, number: u8, name: &str) -> Vec<[u8; MACRO_CHUNK_LEN]> {
        let mut bytes: Vec<u8> = vec![number];
        bytes.extend_from_slice(&RECORD_MAGIC);
        bytes.extend_from_slice(&name_field(GROUP_NAME));
        bytes.extend_from_slice(&name_field(name));
        // At most MACRO_EVENTS_MAX (100) events, so the casts cannot truncate.
        let length: u16 = (self.events.len() * EVENT_LEN) as u16;
        bytes.push(self.events.len() as u8);
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(&[0, 0]);
        for event in &self.events {
            bytes.extend_from_slice(&event.bytes());
        }
        bytes
            .chunks(MACRO_CHUNK_LEN)
            .map(|chunk| {
                let mut padded: [u8; MACRO_CHUNK_LEN] = [0; MACRO_CHUNK_LEN];
                padded[..chunk.len()].copy_from_slice(chunk);
                padded
            })
            .collect()
    }
}

/// `name` as a zero-padded field of printable ASCII, cut to fit.
fn name_field(name: &str) -> [u8; NAME_LEN] {
    let mut field: [u8; NAME_LEN] = [0; NAME_LEN];
    let printable = name
        .bytes()
        .filter(|byte| byte.is_ascii_graphic() || *byte == b' ');
    for (slot, byte) in field.iter_mut().zip(printable.take(NAME_LEN - 1)) {
        *slot = byte;
    }
    field
}

impl fmt::Display for Macro {
    /// One token per event with its delay: `down:ctrl 20ms, down:c 20ms, …`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tokens: Vec<String> = self
            .events
            .iter()
            .map(|event| {
                let direction: &str = if event.pressed { "down" } else { "up" };
                format!("{direction}:{}, {}ms", event.key(), event.delay_ms)
            })
            .collect();
        f.pad(&tokens.join(", "))
    }
}

impl FromStr for Macro {
    type Err = Error;

    /// Parses comma-separated tokens:
    ///
    /// - a key with modifiers, e.g. `ctrl+c` or `a`: the modifiers are pressed,
    ///   the key is pressed and released, the modifiers are released;
    /// - `down:KEY` or `up:KEY`: a single key or modifier pressed or released;
    /// - `NNms`: the delay after the previous event (instead of
    ///   [`DEFAULT_DELAY_MS`]; the last event has no delay unless one is given).
    ///
    /// Every key that is pressed must be released again, after it was pressed.
    fn from_str(s: &str) -> Result<Self> {
        let invalid =
            |reason: String| Error::InvalidValue(format!("invalid macro {s:?}: {reason}"));
        let mut events: Vec<MacroEvent> = Vec::new();
        // Whether the delay of the last event was given explicitly.
        let mut explicit_delay: bool = false;
        for token in s.split(',').map(str::trim) {
            if token.is_empty() {
                return Err(invalid("empty token".to_owned()));
            }
            if let Some(digits) = token.strip_suffix("ms") {
                let Some(last) = events.last_mut() else {
                    return Err(invalid("it starts with a delay".to_owned()));
                };
                if explicit_delay {
                    return Err(invalid("two delays in a row".to_owned()));
                }
                if digits.is_empty() || !digits.bytes().all(|digit| digit.is_ascii_digit()) {
                    return Err(invalid(format!("invalid delay {token:?}")));
                }
                last.delay_ms = digits
                    .parse()
                    .map_err(|_| invalid(format!("delay {token:?} is longer than 65535 ms")))?;
                explicit_delay = true;
                continue;
            }
            let single = |pressed: bool, name: &str| -> Result<MacroEvent> {
                let key: Key = name.parse()?;
                let event = MacroEvent {
                    pressed,
                    modifiers: key.modifiers,
                    usage: key.usage,
                    delay_ms: DEFAULT_DELAY_MS,
                };
                event.check()?;
                Ok(event)
            };
            if let Some(name) = token.strip_prefix("down:") {
                events.push(single(true, name)?);
            } else if let Some(name) = token.strip_prefix("up:") {
                events.push(single(false, name)?);
            } else {
                events.extend(key_strokes(token.parse()?));
            }
            explicit_delay = false;
        }
        if !explicit_delay && let Some(last) = events.last_mut() {
            last.delay_ms = 0;
        }
        // A key left pressed would stay pressed on the computer.
        let mut held: Vec<Key> = Vec::new();
        for event in &events {
            let key: Key = event.key();
            if event.pressed {
                if held.contains(&key) {
                    return Err(invalid(format!("{key} is pressed twice")));
                }
                held.push(key);
            } else if let Some(index) = held.iter().position(|other| *other == key) {
                held.remove(index);
            } else {
                return Err(invalid(format!("{key} is released but not pressed")));
            }
        }
        if let Some(key) = held.first() {
            return Err(invalid(format!(
                "{key} is pressed but not released (add up:{key})"
            )));
        }
        Self::new(events)
    }
}

/// Events for a key with modifiers: modifiers down, key down and up,
/// modifiers up in reverse order.
fn key_strokes(key: Key) -> Vec<MacroEvent> {
    let event = |pressed: bool, modifiers: u8, usage: u8| MacroEvent {
        pressed,
        modifiers,
        usage,
        delay_ms: DEFAULT_DELAY_MS,
    };
    let modifiers: Vec<u8> = MODIFIERS
        .iter()
        .map(|(bit, _)| *bit)
        .filter(|bit| key.modifiers & bit != 0)
        .collect();
    let mut events: Vec<MacroEvent> = modifiers.iter().map(|&bit| event(true, bit, 0)).collect();
    if key.usage != 0 {
        events.push(event(true, 0, key.usage));
        events.push(event(false, 0, key.usage));
    }
    events.extend(modifiers.iter().rev().map(|&bit| event(false, bit, 0)));
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_bytes(text: &str, number: u8, name: &str) -> Vec<u8> {
        let parsed: Macro = text.parse().expect("valid macro");
        parsed.record(number, name).concat()
    }

    #[test]
    fn records_match_the_captures() {
        // Capture 67: "Crouch(Toggle)" = C down, 50 ms, C up, as macro 4.
        let record = record_bytes("down:c, 50ms, up:c", 4, "Crouch(Toggle)");
        assert_eq!(record.len(), 2 * MACRO_CHUNK_LEN);
        assert_eq!(record[..4], [0x04, 0x09, 0x04, 0x38]);
        assert_eq!(&record[4..11], b"konectl");
        assert_eq!(&record[32..46], b"Crouch(Toggle)");
        assert_eq!(
            record[60..75],
            [
                0x02, 0x0a, 0x00, 0x00, 0x00, 0x01, 0x00, 0x06, 0x32, 0x00, 0x81, 0x00, 0x06, 0x00,
                0x00
            ]
        );
        assert!(record[75..].iter().all(|&byte| byte == 0));
        // Capture 72: "Crouch(Hold)" = Ctrl, as macro 3.
        let record = record_bytes("down:ctrl, 50ms, up:ctrl", 3, "Crouch(Hold)");
        assert_eq!(
            record[60..75],
            [
                0x02, 0x0a, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x32, 0x00, 0x81, 0x01, 0x00, 0x00,
                0x00
            ]
        );
    }

    #[test]
    fn long_macros_span_several_chunks() {
        // Capture 73: 42 events = 210 bytes, five chunks; Shift+X as separate events.
        let mut text: Vec<String> = ('a'..='q').map(|letter| letter.to_string()).collect();
        text.push("shift+x".to_owned());
        text.push("ctrl+f5".to_owned());
        let record = record_bytes(&text.join(", "), 4, "konectl test");
        assert_eq!(record.len(), 5 * MACRO_CHUNK_LEN);
        assert_eq!(record[60..65], [0x2a, 0xd2, 0x00, 0x00, 0x00]);
        // Shift down, x down, x up, Shift up.
        let shift_x: usize = 65 + 34 * EVENT_LEN;
        assert_eq!(record[shift_x..shift_x + 3], [0x01, 0x02, 0x00]);
        assert_eq!(record[shift_x + 5..shift_x + 8], [0x01, 0x00, 0x1b]);
        assert_eq!(record[shift_x + 15..shift_x + 18], [0x81, 0x02, 0x00]);
    }

    #[test]
    fn parses_keys_and_delays() {
        let parsed: Macro = "ctrl+c, 50ms, ctrl+v".parse().expect("macro");
        let events = parsed.events();
        assert_eq!(events.len(), 8);
        assert_eq!(
            events[0],
            MacroEvent {
                pressed: true,
                modifiers: 0x01,
                usage: 0,
                delay_ms: DEFAULT_DELAY_MS
            }
        );
        assert_eq!(events[3].delay_ms, 50);
        assert_eq!(events[7].delay_ms, 0);
        // A trailing delay stays (it separates repetitions).
        let parsed: Macro = "a, 100ms".parse().expect("macro");
        assert_eq!(parsed.events()[1].delay_ms, 100);
        assert_eq!(parsed.to_string(), "down:a, 20ms, up:a, 100ms");
        for bad in [
            "",
            "50ms, a",
            "a, 50ms, 60ms",
            "a,,b",
            "a, 70000ms",
            "a, -5ms",
            "down:ctrl+c",
            "teleport",
            "a, 1.5ms",
            "down:ctrl, a",
            "up:a",
            "down:a, down:a, up:a",
        ] {
            assert!(bad.parse::<Macro>().is_err(), "{bad:?}");
        }
        let too_long: String = vec!["a"; MACRO_EVENTS_MAX / 2 + 1].join(", ");
        assert!(too_long.parse::<Macro>().is_err());
        let longest: String = vec!["a"; MACRO_EVENTS_MAX / 2].join(", ");
        assert!(longest.parse::<Macro>().is_ok());
    }

    #[test]
    fn names_are_cut_and_cleaned() {
        assert_eq!(name_field("a\u{e9}b\nc")[..4], *b"abc\0");
        assert_eq!(name_field(&"x".repeat(40))[NAME_LEN - 1], 0);
    }
}
