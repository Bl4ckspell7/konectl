//! RGB colour value used for the mouse LEDs.

use std::fmt;
use std::str::FromStr;

/// An 8-bit-per-channel RGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    /// LEDs off.
    pub const BLACK: Self = Self::new(0, 0, 0);

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Returns the colour dimmed to `brightness` percent (values above 100 count as 100).
    pub fn scaled(self, brightness: u8) -> Self {
        let percent: u16 = u16::from(brightness.min(100));
        // Result is at most 255 * 100 / 100, so it always fits into a u8.
        let scale = |channel: u8| -> u8 { (u16::from(channel) * percent / 100) as u8 };
        Self::new(scale(self.r), scale(self.g), scale(self.b))
    }

    /// The colour as `[r, g, b]`, the order the mouse expects.
    pub const fn to_bytes(self) -> [u8; 3] {
        [self.r, self.g, self.b]
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// Error returned when a string is not a valid `rrggbb` colour.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid colour {input:?}: expected 6 hex digits like ff8800 or #ff8800")]
pub struct ParseRgbError {
    input: String,
}

impl FromStr for Rgb {
    type Err = ParseRgbError;

    /// Parses `rrggbb` or `#rrggbb` (case-insensitive).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || ParseRgbError {
            input: s.to_owned(),
        };
        let hex: &str = s.strip_prefix('#').unwrap_or(s);
        if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        let channel =
            |start: usize| u8::from_str_radix(&hex[start..start + 2], 16).map_err(|_| invalid());
        Ok(Self::new(channel(0)?, channel(2)?, channel(4)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_with_and_without_hash() {
        assert_eq!("ff8800".parse(), Ok(Rgb::new(0xff, 0x88, 0x00)));
        assert_eq!("#00FF7f".parse(), Ok(Rgb::new(0x00, 0xff, 0x7f)));
    }

    #[test]
    fn rejects_invalid_input() {
        for input in ["", "#", "fff", "ff88001", "gg0000", "#ff 800", "ü12345"] {
            assert!(input.parse::<Rgb>().is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn display_round_trips() {
        let colour = Rgb::new(1, 0xab, 0xff);
        assert_eq!(colour.to_string(), "#01abff");
        assert_eq!(colour.to_string().parse(), Ok(colour));
    }

    #[test]
    fn scales_brightness() {
        let colour = Rgb::new(255, 100, 1);
        assert_eq!(colour.scaled(100), colour);
        assert_eq!(colour.scaled(200), colour);
        assert_eq!(colour.scaled(50), Rgb::new(127, 50, 0));
        assert_eq!(colour.scaled(0), Rgb::BLACK);
    }
}
