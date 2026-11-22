//! Button assignments: settings blocks 2, 3 and 4.
//!
//! Every button has a function in two layers: the standard layer, and the
//! Easy-Shift[+] layer that applies while a button with the Easy-Shift function
//! is held. Block 2 holds a function code per button and layer. For code `40`
//! the button's three-byte entry in block 3 (standard layer) or block 4
//! (Easy-Shift layer) says what the button sends; other codes are functions of
//! the mouse itself and have a zero entry. See `docs/protocol.md`.

use std::fmt;
use std::str::FromStr;

use crate::error::{Error, Result};
use crate::settings::DPI_STEP;

/// Number of buttons, numbered as in the official software.
pub const BUTTON_COUNT: u8 = 8;
/// Lowest custom Easy-Aim DPI the official software offers.
pub const EASY_AIM_DPI_MIN: u16 = 100;
/// Highest custom Easy-Aim DPI the official software offers.
pub const EASY_AIM_DPI_MAX: u16 = 19_000;

/// Length of block 2 (function codes).
pub(crate) const FUNCTIONS_LEN: usize = 28;
/// Length of blocks 3 and 4 (entries).
pub(crate) const ENTRIES_LEN: usize = 24;
const ENTRY_LEN: usize = 3;
/// Block 2 holds the standard layer's codes at 0–7, the Easy-Shift layer's at 8–15.
const EASY_SHIFT_CODES: usize = 8;

/// Function code: the button sends what its entry says.
const CODE_ENTRY: u8 = 0x40;
const CODE_DISABLED: u8 = 0x00;

/// First entry byte (kind) for [`CODE_ENTRY`].
const ENTRY_MOUSE: u8 = 0x00;
const ENTRY_KEY: u8 = 0x01;
const ENTRY_CONSUMER: u8 = 0x02;
const ENTRY_SYSTEM: u8 = 0x05;
const ENTRY_SWARM: u8 = 0x0b;
const ENTRY_EASY_AIM: u8 = 0x0c;
const ZERO_ENTRY: [u8; ENTRY_LEN] = [0; ENTRY_LEN];

/// Slot (0-based) of buttons 1–8 in the blocks.
const SLOTS: [usize; BUTTON_COUNT as usize] = [0, 1, 2, 6, 7, 4, 3, 5];
const LEFT_BUTTON: u8 = 1;
const WHEEL_UP: u8 = 4;
const WHEEL_DOWN: u8 = 5;
const PROFILE_BUTTON: u8 = 8;

/// The two layers of button functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Standard,
    /// While a button with the [`Special::EasyShift`] function is held.
    EasyShift,
}

/// The button assignments of one profile.
///
/// Keeps the raw blocks it was read from; bytes konectl does not understand
/// are written back unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Buttons {
    profile: u8,
    functions: [u8; FUNCTIONS_LEN],
    standard: [u8; ENTRIES_LEN],
    easy_shift: [u8; ENTRIES_LEN],
}

impl Buttons {
    /// Wraps blocks 2, 3 and 4 of `profile`.
    pub(crate) const fn from_blocks(
        profile: u8,
        functions: [u8; FUNCTIONS_LEN],
        standard: [u8; ENTRIES_LEN],
        easy_shift: [u8; ENTRIES_LEN],
    ) -> Self {
        Self {
            profile,
            functions,
            standard,
            easy_shift,
        }
    }

    /// Profile number (1–5) the assignments belong to.
    pub const fn profile(&self) -> u8 {
        self.profile
    }

    /// Block 2: function codes.
    pub(crate) const fn functions(&self) -> &[u8; FUNCTIONS_LEN] {
        &self.functions
    }

    /// Block 3 (standard layer) or 4 (Easy-Shift layer): entries.
    pub(crate) const fn entries(&self, layer: Layer) -> &[u8; ENTRIES_LEN] {
        match layer {
            Layer::Standard => &self.standard,
            Layer::EasyShift => &self.easy_shift,
        }
    }

    /// What `button` (1–8) does in `layer`.
    pub fn action(&self, button: u8, layer: Layer) -> Result<ButtonAction> {
        let slot: usize = slot(button)?;
        let entries: &[u8; ENTRIES_LEN] = self.entries(layer);
        let mut entry: [u8; ENTRY_LEN] = ZERO_ENTRY;
        entry.copy_from_slice(&entries[slot * ENTRY_LEN..(slot + 1) * ENTRY_LEN]);
        Ok(ButtonAction::decode(
            self.functions[code_index(slot, layer)],
            entry,
        ))
    }

    /// `false` where the official software offers no Easy-Shift function: on
    /// button 8 and on buttons that are themselves Easy-Shift buttons.
    pub fn has_easy_shift_function(&self, button: u8) -> Result<bool> {
        Ok(button != PROFILE_BUTTON
            && self.action(button, Layer::Standard)? != ButtonAction::Special(Special::EasyShift))
    }

    /// Assigns `action` to `button` (1–8) in `layer`, within the limits of the
    /// official software:
    ///
    /// - button 1 keeps its click in the standard layer (so the mouse stays usable);
    /// - button 8 can only switch DPI stages or profiles, and has no Easy-Shift function;
    /// - the wheel (buttons 4 and 5) cannot be held, so it takes no Easy-Shift,
    ///   Easy-Aim or Easy-Wheel;
    /// - the Easy-Shift layer has no Easy-Shift function, and an Easy-Shift
    ///   button has no function in it (setting Easy-Shift disables it).
    ///
    /// [`ButtonAction::Host`] and [`ButtonAction::Other`] cannot be set.
    pub fn set_action(&mut self, button: u8, layer: Layer, action: ButtonAction) -> Result<()> {
        let slot: usize = slot(button)?;
        action.check()?;
        let refuse = |reason: String| Err(Error::InvalidValue(reason));
        if (button == WHEEL_UP || button == WHEEL_DOWN) && action.needs_holding() {
            return refuse(format!(
                "button {button} is the wheel, which cannot be held: {action} needs a button"
            ));
        }
        match layer {
            Layer::Standard => {
                if button == LEFT_BUTTON && action != ButtonAction::Mouse(MouseFunction::Click) {
                    return refuse(
                        "button 1 (left) stays click, so the mouse stays usable".to_owned(),
                    );
                }
                if button == PROFILE_BUTTON && !action.fits_profile_button() {
                    return refuse(
                        "button 8 can only switch DPI stages or profiles: dpi-cycle, dpi-up, \
                         dpi-down, profile-cycle, profile-next or profile-previous"
                            .to_owned(),
                    );
                }
            }
            Layer::EasyShift => {
                if !self.has_easy_shift_function(button)? {
                    return refuse(format!("button {button} has no Easy-Shift function"));
                }
                if action == ButtonAction::Special(Special::EasyShift) {
                    return refuse("Easy-Shift cannot be set in the Easy-Shift layer".to_owned());
                }
            }
        }

        self.store(slot, layer, action);
        if layer == Layer::Standard && action == ButtonAction::Special(Special::EasyShift) {
            // Like the official software: an Easy-Shift button does nothing
            // else while Easy-Shift is held.
            self.store(slot, Layer::EasyShift, ButtonAction::Disabled);
        }
        Ok(())
    }

    fn store(&mut self, slot: usize, layer: Layer, action: ButtonAction) {
        let (code, entry): (u8, [u8; ENTRY_LEN]) = action.encode();
        self.functions[code_index(slot, layer)] = code;
        let entries: &mut [u8; ENTRIES_LEN] = match layer {
            Layer::Standard => &mut self.standard,
            Layer::EasyShift => &mut self.easy_shift,
        };
        entries[slot * ENTRY_LEN..(slot + 1) * ENTRY_LEN].copy_from_slice(&entry);
    }
}

/// Checks a button number (1–8) and returns its slot in the blocks.
fn slot(button: u8) -> Result<usize> {
    if (1..=BUTTON_COUNT).contains(&button) {
        Ok(SLOTS[usize::from(button - 1)])
    } else {
        Err(Error::InvalidValue(format!(
            "button {button} does not exist (1–{BUTTON_COUNT})"
        )))
    }
}

/// Offset of a slot's function code in block 2.
const fn code_index(slot: usize, layer: Layer) -> usize {
    match layer {
        Layer::Standard => slot,
        Layer::EasyShift => EASY_SHIFT_CODES + slot,
    }
}

/// What a button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonAction {
    /// Does nothing.
    Disabled,
    /// A mouse button or wheel function.
    Mouse(MouseFunction),
    /// A key, optionally with modifiers, or modifiers alone.
    Key(Key),
    /// A consumer control (media or browser key), as a HID usage.
    Consumer(u16),
    /// A function of the mouse itself (DPI, profiles, Easy-Shift, …).
    Special(Special),
    /// Easy-Aim with a custom DPI: the DPI while the button is held.
    EasyAim(u16),
    /// A function the official software carries out on the computer (open a
    /// program, lock the screen, …); the mouse only reports the press.
    Host(HostFunction),
    /// A combination konectl does not know.
    Other { code: u8, entry: [u8; ENTRY_LEN] },
}

impl ButtonAction {
    fn decode(code: u8, entry: [u8; ENTRY_LEN]) -> Self {
        let other = Self::Other { code, entry };
        match (code, entry) {
            (CODE_DISABLED, ZERO_ENTRY) => Self::Disabled,
            (CODE_ENTRY, [ENTRY_MOUSE, function, 0]) => {
                MouseFunction::from_code(function).map_or(other, Self::Mouse)
            }
            (CODE_ENTRY, [ENTRY_KEY, modifiers, usage]) if modifiers != 0 || usage != 0 => {
                Self::Key(Key { modifiers, usage })
            }
            (CODE_ENTRY, [ENTRY_CONSUMER, low, high]) => {
                let usage: u16 = u16::from_le_bytes([low, high]);
                if (1..=CONSUMER_USAGE_MAX).contains(&usage) {
                    Self::Consumer(usage)
                } else {
                    other
                }
            }
            (CODE_ENTRY, [ENTRY_SYSTEM, id, 0]) => Self::Host(HostFunction::System(id)),
            (CODE_ENTRY, [ENTRY_SWARM, id, 0]) => Self::Host(HostFunction::Swarm(id)),
            (CODE_ENTRY, [ENTRY_EASY_AIM, low, high]) => {
                match u16::from_le_bytes([low, high]).checked_mul(DPI_STEP) {
                    Some(dpi) if dpi > 0 => Self::EasyAim(dpi),
                    _ => other,
                }
            }
            (_, ZERO_ENTRY) => Special::from_code(code).map_or(other, Self::Special),
            _ => other,
        }
    }

    /// Function code and entry.
    fn encode(self) -> (u8, [u8; ENTRY_LEN]) {
        match self {
            Self::Disabled => (CODE_DISABLED, ZERO_ENTRY),
            Self::Mouse(function) => (CODE_ENTRY, [ENTRY_MOUSE, function.code(), 0]),
            Self::Key(key) => (CODE_ENTRY, [ENTRY_KEY, key.modifiers, key.usage]),
            Self::Consumer(usage) => {
                let [low, high] = usage.to_le_bytes();
                (CODE_ENTRY, [ENTRY_CONSUMER, low, high])
            }
            Self::Special(special) => (special.code(), ZERO_ENTRY),
            Self::EasyAim(dpi) => {
                let [low, high] = (dpi / DPI_STEP).to_le_bytes();
                (CODE_ENTRY, [ENTRY_EASY_AIM, low, high])
            }
            Self::Host(HostFunction::System(id)) => (CODE_ENTRY, [ENTRY_SYSTEM, id, 0]),
            Self::Host(HostFunction::Swarm(id)) => (CODE_ENTRY, [ENTRY_SWARM, id, 0]),
            Self::Other { code, entry } => (code, entry),
        }
    }

    /// Fails unless the action is one konectl sets, with valid values.
    fn check(self) -> Result<()> {
        let invalid = |message: String| Err(Error::InvalidValue(message));
        match self {
            Self::Disabled | Self::Mouse(_) => Ok(()),
            Self::Key(key) => key.check(),
            Self::Consumer(usage) if usage == 0 || usage > CONSUMER_USAGE_MAX => invalid(format!(
                "invalid consumer usage {usage:#06x}: use 0x0001 to {CONSUMER_USAGE_MAX:#06x}"
            )),
            Self::Consumer(_) => Ok(()),
            Self::Special(Special::Profile(profile))
                if !(1..=PRESET_PROFILES).contains(&profile) =>
            {
                invalid(format!(
                    "profile:{profile} is not supported: use profile:1 to profile:{PRESET_PROFILES}"
                ))
            }
            Self::Special(_) => Ok(()),
            Self::EasyAim(dpi)
                if !(EASY_AIM_DPI_MIN..=EASY_AIM_DPI_MAX).contains(&dpi)
                    || !dpi.is_multiple_of(DPI_STEP) =>
            {
                invalid(format!(
                    "invalid Easy-Aim DPI {dpi}: use a multiple of {DPI_STEP} from \
                     {EASY_AIM_DPI_MIN} to {EASY_AIM_DPI_MAX}"
                ))
            }
            Self::EasyAim(_) => Ok(()),
            Self::Host(_) => invalid(format!(
                "{self} is carried out by the official software, not by the mouse; konectl does \
                 not set it"
            )),
            Self::Other { .. } => invalid(format!("{self} cannot be set")),
        }
    }

    /// Functions that act while the button is held, which the wheel cannot.
    fn needs_holding(self) -> bool {
        matches!(
            self,
            Self::EasyAim(_)
                | Self::Special(
                    Special::EasyShift
                        | Special::EasyAim200
                        | Special::EasyAim900
                        | Special::EasyWheelDpi
                        | Special::EasyWheelVolume
                        | Special::EasyWheelAltTab
                        | Special::EasyWheelFlip3d
                )
        )
    }

    /// The functions the official software offers for button 8.
    fn fits_profile_button(self) -> bool {
        matches!(
            self,
            Self::Special(
                Special::DpiCycle
                    | Special::DpiUp
                    | Special::DpiDown
                    | Special::ProfileCycle
                    | Special::ProfileNext
                    | Special::ProfilePrevious
            )
        )
    }
}

impl fmt::Display for ButtonAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text: String = match self {
            Self::Disabled => "disabled".to_owned(),
            Self::Mouse(function) => function.name().to_owned(),
            Self::Key(key) => format!("key:{key}"),
            Self::Consumer(usage) => consumer_name(*usage)
                .map_or_else(|| format!("consumer:{usage:#06x}"), str::to_owned),
            Self::Special(special) => special.to_string(),
            Self::EasyAim(dpi) => format!("easy-aim:{dpi}"),
            Self::Host(function) => function.to_string(),
            Self::Other { code, entry } => format!(
                "unknown ({code:02x} {:02x} {:02x} {:02x})",
                entry[0], entry[1], entry[2]
            ),
        };
        f.pad(&text)
    }
}

impl FromStr for ButtonAction {
    type Err = Error;

    /// Parses the names [`ButtonAction`]'s `Display` writes: `disabled`, a
    /// mouse function (`click`, `back`, …), `key:ctrl+c`, a consumer control
    /// (`volume-up`, `consumer:0x0223`), a special function (`dpi-up`,
    /// `profile:2`, …) or `easy-aim:<dpi>`.
    fn from_str(s: &str) -> Result<Self> {
        let unknown = || {
            Error::InvalidValue(format!(
                "unknown button function {s:?}; see `konectl button --help` for the list"
            ))
        };
        if s == "disabled" {
            return Ok(Self::Disabled);
        }
        if let Some(function) = MouseFunction::ALL
            .into_iter()
            .find(|function| function.name() == s)
        {
            return Ok(Self::Mouse(function));
        }
        if let Some(key) = s.strip_prefix("key:") {
            return key.parse().map(Self::Key);
        }
        if let Some((usage, _)) = CONSUMER_NAMES.iter().find(|(_, name)| *name == s) {
            return Ok(Self::Consumer(*usage));
        }
        if let Some(usage) = s.strip_prefix("consumer:") {
            return parse_hex_u16(usage)
                .map(Self::Consumer)
                .ok_or_else(|| Error::InvalidValue(format!("invalid consumer usage {usage:?}")));
        }
        if let Some(dpi) = s.strip_prefix("easy-aim:") {
            return parse_decimal(dpi)
                .map(Self::EasyAim)
                .ok_or_else(|| Error::InvalidValue(format!("invalid Easy-Aim DPI {dpi:?}")));
        }
        if let Some(profile) = s.strip_prefix("profile:") {
            return parse_decimal(profile)
                .map(|profile| Self::Special(Special::Profile(profile)))
                .ok_or_else(|| Error::InvalidValue(format!("invalid profile {profile:?}")));
        }
        Special::NAMED
            .into_iter()
            .find(|special| special.to_string() == s)
            .map(Self::Special)
            .ok_or_else(unknown)
    }
}

/// Mouse button and wheel functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseFunction {
    Click,
    /// The right click ("Menu" in the official software).
    Menu,
    /// Middle click ("Universal scrolling" in the official software).
    UniversalScroll,
    /// Browser back (mouse button 4).
    Back,
    /// Browser forward (mouse button 5).
    Forward,
    DoubleClick,
    ScrollUp,
    ScrollDown,
}

impl MouseFunction {
    const ALL: [Self; 8] = [
        Self::Click,
        Self::Menu,
        Self::UniversalScroll,
        Self::Back,
        Self::Forward,
        Self::DoubleClick,
        Self::ScrollUp,
        Self::ScrollDown,
    ];

    const fn code(self) -> u8 {
        match self {
            Self::Click => 0x01,
            Self::Menu => 0x02,
            Self::UniversalScroll => 0x03,
            Self::Back => 0x04,
            Self::Forward => 0x05,
            Self::DoubleClick => 0x06,
            Self::ScrollUp => 0x07,
            Self::ScrollDown => 0x08,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|function| function.code() == code)
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Click => "click",
            Self::Menu => "menu",
            Self::UniversalScroll => "universal-scroll",
            Self::Back => "back",
            Self::Forward => "forward",
            Self::DoubleClick => "double-click",
            Self::ScrollUp => "scroll-up",
            Self::ScrollDown => "scroll-down",
        }
    }
}

/// Functions of the mouse itself, stored as a function code in block 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Special {
    /// Hold for the Easy-Shift layer ("Easy-Shift[+]" and "Easy-Shift[+] All
    /// Devices" in the official software).
    EasyShift,
    DpiCycle,
    DpiUp,
    DpiDown,
    ProfileCycle,
    ProfileNext,
    ProfilePrevious,
    /// Switch to profile 1–3 ("Profile presets").
    Profile(u8),
    /// Hold and turn the wheel to change the DPI.
    EasyWheelDpi,
    /// Hold and turn the wheel to change the volume.
    EasyWheelVolume,
    /// Hold and turn the wheel to switch windows (Alt+Tab).
    EasyWheelAltTab,
    /// Hold and turn the wheel for the "Aero Flip 3D" window switcher.
    EasyWheelFlip3d,
    /// Easy-Aim preset: 200 DPI while held.
    EasyAim200,
    /// Easy-Aim preset: 900 DPI while held.
    EasyAim900,
}

/// Profiles whose "Profile presets" codes are known (`48`–`4a`).
const PRESET_PROFILES: u8 = 3;
const CODE_PROFILE_1: u8 = 0x48;

impl Special {
    /// The functions with a fixed name (all but [`Special::Profile`]).
    const NAMED: [Self; 13] = [
        Self::EasyShift,
        Self::DpiCycle,
        Self::DpiUp,
        Self::DpiDown,
        Self::ProfileCycle,
        Self::ProfileNext,
        Self::ProfilePrevious,
        Self::EasyWheelDpi,
        Self::EasyWheelVolume,
        Self::EasyWheelAltTab,
        Self::EasyWheelFlip3d,
        Self::EasyAim200,
        Self::EasyAim900,
    ];

    const fn code(self) -> u8 {
        match self {
            Self::ProfileCycle => 0x41,
            Self::DpiCycle => 0x42,
            Self::EasyShift => 0x45,
            Self::ProfileNext => 0x46,
            Self::ProfilePrevious => 0x47,
            Self::Profile(profile @ 1..=PRESET_PROFILES) => CODE_PROFILE_1 + profile - 1,
            Self::Profile(_) => panic!("profiles are checked before they are encoded"),
            Self::EasyWheelDpi => 0x4e,
            Self::EasyWheelVolume => 0x4f,
            Self::EasyWheelAltTab => 0x50,
            Self::EasyWheelFlip3d => 0x51,
            Self::DpiUp => 0x52,
            Self::DpiDown => 0x53,
            Self::EasyAim200 => 0x54,
            Self::EasyAim900 => 0x56,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        if (CODE_PROFILE_1..CODE_PROFILE_1 + PRESET_PROFILES).contains(&code) {
            return Some(Self::Profile(code - CODE_PROFILE_1 + 1));
        }
        Self::NAMED
            .into_iter()
            .find(|special| special.code() == code)
    }
}

impl fmt::Display for Special {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name: &str = match self {
            Self::EasyShift => "easy-shift",
            Self::DpiCycle => "dpi-cycle",
            Self::DpiUp => "dpi-up",
            Self::DpiDown => "dpi-down",
            Self::ProfileCycle => "profile-cycle",
            Self::ProfileNext => "profile-next",
            Self::ProfilePrevious => "profile-previous",
            Self::Profile(profile) => return f.pad(&format!("profile:{profile}")),
            Self::EasyWheelDpi => "easy-wheel-dpi",
            Self::EasyWheelVolume => "easy-wheel-volume",
            Self::EasyWheelAltTab => "easy-wheel-alt-tab",
            Self::EasyWheelFlip3d => "easy-wheel-flip-3d",
            Self::EasyAim200 => "easy-aim-200",
            Self::EasyAim900 => "easy-aim-900",
        };
        f.pad(name)
    }
}

/// A function the official software carries out when the mouse reports the
/// press (`90 07`). konectl shows and keeps these but does not set them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostFunction {
    /// Entry kind `05`: sleep, wake up, shut down.
    System(u8),
    /// Entry kind `0b`: open programs, documents or websites, lock the
    /// screen, timers, Windows tools.
    Swarm(u8),
}

const SYSTEM_NAMES: [(u8, &str); 3] = [(0x74, "shut-down"), (0x8e, "sleep"), (0x8f, "wake")];
const SWARM_NAMES: [(u8, &str); 8] = [
    (0x02, "open-swarm"),
    (0x04, "stop-timer"),
    (0x07, "open-website"),
    (0x23, "hibernate"),
    (0x24, "reboot"),
    (0x25, "lock"),
    (0x26, "log-off"),
    (0x29, "task-manager"),
];

impl fmt::Display for HostFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (prefix, id, names): (&str, u8, &[(u8, &str)]) = match self {
            Self::System(id) => ("system", *id, &SYSTEM_NAMES),
            Self::Swarm(id) => ("swarm", *id, &SWARM_NAMES),
        };
        let text: String = match names.iter().find(|(code, _)| *code == id) {
            Some((_, name)) => format!("{prefix}:{name}"),
            None => format!("{prefix}:{id:#04x}"),
        };
        f.pad(&text)
    }
}

/// Highest consumer usage konectl accepts.
const CONSUMER_USAGE_MAX: u16 = 0x0fff;
const CONSUMER_NAMES: [(u16, &str); 10] = [
    (0x00b5, "next-track"),
    (0x00b6, "previous-track"),
    (0x00b7, "stop"),
    (0x00cd, "play-pause"),
    (0x00e2, "mute"),
    (0x00e9, "volume-up"),
    (0x00ea, "volume-down"),
    (0x0221, "browser-search"),
    (0x0226, "browser-stop"),
    (0x0227, "browser-refresh"),
];

fn consumer_name(usage: u16) -> Option<&'static str> {
    CONSUMER_NAMES
        .iter()
        .find(|(code, _)| *code == usage)
        .map(|(_, name)| *name)
}

/// `0x`-prefixed hex with up to four digits.
fn parse_hex_u16(text: &str) -> Option<u16> {
    let digits: &str = text.strip_prefix("0x")?;
    if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|d| d.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(digits, 16).ok()
}

/// Decimal digits only (no sign or spaces, which `str::parse` would accept).
fn parse_decimal<T: FromStr>(text: &str) -> Option<T> {
    if text.is_empty() || !text.bytes().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A key press: HID keyboard usage plus a modifier mask (both optional, but
/// not both empty).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    /// HID modifier mask: `01` Ctrl, `02` Shift, `04` Alt, `08` Super (left
    /// keys; the right ones are `10`–`80`).
    pub modifiers: u8,
    /// HID keyboard usage, e.g. `0x06` for C; `0` for modifiers alone.
    pub usage: u8,
}

/// Modifier bits and names, in the order they are written.
const MODIFIERS: [(u8, &str); 8] = [
    (0x01, "ctrl"),
    (0x02, "shift"),
    (0x04, "alt"),
    (0x08, "super"),
    (0x10, "right-ctrl"),
    (0x20, "right-shift"),
    (0x40, "right-alt"),
    (0x80, "right-super"),
];
/// The modifiers the official software offers (the left keys).
const SETTABLE_MODIFIERS: u8 = 0x0f;
/// HID usages of the modifier keys; they are set through the mask instead.
const MODIFIER_USAGES: std::ops::RangeInclusive<u8> = 0xe0..=0xe7;
/// Usages 1–3 are error codes, not keys.
const FIRST_KEY_USAGE: u8 = 0x04;

const NAMED_KEYS: [(u8, &str); 41] = [
    (0x28, "enter"),
    (0x29, "esc"),
    (0x2a, "backspace"),
    (0x2b, "tab"),
    (0x2c, "space"),
    (0x2d, "minus"),
    (0x2e, "equal"),
    (0x2f, "left-bracket"),
    (0x30, "right-bracket"),
    (0x31, "backslash"),
    (0x33, "semicolon"),
    (0x34, "apostrophe"),
    (0x35, "grave"),
    (0x36, "comma"),
    (0x37, "period"),
    (0x38, "slash"),
    (0x39, "caps-lock"),
    (0x46, "print-screen"),
    (0x47, "scroll-lock"),
    (0x48, "pause"),
    (0x49, "insert"),
    (0x4a, "home"),
    (0x4b, "page-up"),
    (0x4c, "delete"),
    (0x4d, "end"),
    (0x4e, "page-down"),
    (0x4f, "right"),
    (0x50, "left"),
    (0x51, "down"),
    (0x52, "up"),
    (0x53, "num-lock"),
    (0x54, "kp-divide"),
    (0x55, "kp-multiply"),
    (0x56, "kp-minus"),
    (0x57, "kp-plus"),
    (0x58, "kp-enter"),
    (0x62, "kp-0"),
    (0x63, "kp-period"),
    (0x65, "application"),
    (0x2c, "spacebar"),
    (0x29, "escape"),
];

impl Key {
    fn check(self) -> Result<()> {
        let invalid = |message: String| Err(Error::InvalidValue(message));
        if self.modifiers == 0 && self.usage == 0 {
            return invalid("a key needs a key or a modifier".to_owned());
        }
        if self.modifiers & !SETTABLE_MODIFIERS != 0 {
            return invalid("only ctrl, shift, alt and super can be used as modifiers".to_owned());
        }
        if self.usage != 0
            && (self.usage < FIRST_KEY_USAGE || MODIFIER_USAGES.contains(&self.usage))
        {
            return invalid(format!(
                "invalid key usage {:#04x}: use a key, and modifiers by name",
                self.usage
            ));
        }
        Ok(())
    }
}

/// Name of a key usage: letters, digits, F-keys, keypad digits and the
/// named keys; `None` for other usages.
fn key_name(usage: u8) -> Option<String> {
    match usage {
        0x04..=0x1d => Some(char::from(b'a' + (usage - 0x04)).to_string()),
        0x1e..=0x26 => Some(char::from(b'1' + (usage - 0x1e)).to_string()),
        0x27 => Some("0".to_owned()),
        0x3a..=0x45 => Some(format!("f{}", usage - 0x3a + 1)),
        0x68..=0x73 => Some(format!("f{}", usage - 0x68 + 13)),
        0x59..=0x61 => Some(format!("kp-{}", usage - 0x59 + 1)),
        _ => NAMED_KEYS
            .iter()
            .find(|(code, _)| *code == usage)
            .map(|(_, name)| (*name).to_owned()),
    }
}

/// Usage of a key name written by [`key_name`] (plus a few aliases) or `0xNN`.
fn key_usage(name: &str) -> Option<u8> {
    if let Some(hex) = name.strip_prefix("0x") {
        return parse_hex_u16(name)
            .filter(|_| hex.len() <= 2)
            .and_then(|usage| u8::try_from(usage).ok());
    }
    (FIRST_KEY_USAGE..=0x73)
        .find(|&usage| key_name(usage).as_deref() == Some(name))
        .or_else(|| {
            NAMED_KEYS
                .iter()
                .find(|(_, alias)| *alias == name)
                .map(|(code, _)| *code)
        })
}

fn modifier_bit(name: &str) -> Option<u8> {
    match name {
        "win" | "meta" => Some(0x08),
        "control" => Some(0x01),
        _ => MODIFIERS
            .iter()
            .find(|(_, modifier)| *modifier == name)
            .map(|(bit, _)| *bit),
    }
}

impl fmt::Display for Key {
    /// Modifiers, then the key, joined by `+`: `ctrl+shift+f5`, `ctrl`, `home`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<String> = MODIFIERS
            .iter()
            .filter(|(bit, _)| self.modifiers & bit != 0)
            .map(|(_, name)| (*name).to_owned())
            .collect();
        if self.usage != 0 {
            parts.push(key_name(self.usage).unwrap_or_else(|| format!("{:#04x}", self.usage)));
        }
        f.pad(&parts.join("+"))
    }
}

impl FromStr for Key {
    type Err = Error;

    /// Parses `ctrl+c`, `shift+alt+f5`, `home`, `ctrl` or `0x32`. Modifiers:
    /// ctrl, shift, alt, super (also win or meta).
    fn from_str(s: &str) -> Result<Self> {
        let invalid = |reason: &str| Error::InvalidValue(format!("invalid key {s:?}: {reason}"));
        let mut key = Self {
            modifiers: 0,
            usage: 0,
        };
        for part in s.split('+') {
            if let Some(bit) = modifier_bit(part) {
                if key.modifiers & bit != 0 {
                    return Err(invalid("a modifier is given twice"));
                }
                key.modifiers |= bit;
            } else if key.usage != 0 {
                return Err(invalid("only one key can be combined with modifiers"));
            } else {
                key.usage = key_usage(part).ok_or_else(|| invalid("unknown key name"))?;
            }
        }
        key.check()?;
        Ok(key)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Blocks 2–4 of profile 1 when the capture session started (capture 48):
    /// 1 click, 2 menu, 3 universal scrolling, 4/5 scroll, 6 Easy-Shift,
    /// 7 the `'` key, 8 next profile; Easy-Shift: 1 key 7, 2 key 8, 3 mute,
    /// 4 volume up, 5 volume down, 7 disabled.
    pub(crate) fn start_buttons() -> Buttons {
        let mut functions: [u8; FUNCTIONS_LEN] = [0; FUNCTIONS_LEN];
        functions[..16].copy_from_slice(&[
            0x40, 0x40, 0x40, 0x40, 0x45, 0x46, 0x40, 0x40, 0x40, 0x40, 0x40, 0x00, 0x00, 0x00,
            0x40, 0x40,
        ]);
        let standard: [u8; ENTRIES_LEN] = [
            0x00, 0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x34, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x08, 0x00,
        ];
        let easy_shift: [u8; ENTRIES_LEN] = [
            0x01, 0x00, 0x24, 0x01, 0x00, 0x25, 0x02, 0xe2, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x02, 0xe9, 0x00, 0x02, 0xea, 0x00,
        ];
        Buttons::from_blocks(1, functions, standard, easy_shift)
    }

    fn actions(buttons: &Buttons, layer: Layer) -> Vec<String> {
        (1..=BUTTON_COUNT)
            .map(|button| {
                buttons
                    .action(button, layer)
                    .expect("valid button")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn decodes_the_captured_assignments() {
        let buttons = start_buttons();
        assert_eq!(
            actions(&buttons, Layer::Standard),
            [
                "click",
                "menu",
                "universal-scroll",
                "scroll-up",
                "scroll-down",
                "easy-shift",
                "key:apostrophe",
                "profile-next"
            ]
        );
        assert_eq!(
            actions(&buttons, Layer::EasyShift),
            [
                "key:7",
                "key:8",
                "mute",
                "volume-up",
                "volume-down",
                "disabled",
                "disabled",
                "disabled"
            ]
        );
        assert!(!buttons.has_easy_shift_function(6).expect("button 6"));
        assert!(!buttons.has_easy_shift_function(8).expect("button 8"));
        assert!(buttons.has_easy_shift_function(7).expect("button 7"));
    }

    /// Sets `action` and returns block 2 and the button's entry.
    fn set(button: u8, layer: Layer, action: &str) -> (Buttons, [u8; ENTRY_LEN]) {
        let mut buttons = start_buttons();
        let action: ButtonAction = action.parse().expect("valid action");
        buttons
            .set_action(button, layer, action)
            .expect("allowed action");
        let slot: usize = SLOTS[usize::from(button - 1)];
        let mut entry: [u8; ENTRY_LEN] = ZERO_ENTRY;
        entry.copy_from_slice(&buttons.entries(layer)[slot * 3..slot * 3 + 3]);
        (buttons, entry)
    }

    #[test]
    fn encodes_like_the_official_software() {
        // (button, layer, action, function code, entry) from captures 51–56.
        let cases: [(u8, Layer, &str, u8, [u8; 3]); 22] = [
            (3, Layer::Standard, "double-click", 0x40, [0x00, 0x06, 0x00]),
            (4, Layer::Standard, "dpi-up", 0x52, ZERO_ENTRY),
            (5, Layer::Standard, "dpi-down", 0x53, ZERO_ENTRY),
            (7, Layer::Standard, "profile-cycle", 0x41, ZERO_ENTRY),
            (8, Layer::Standard, "profile-previous", 0x47, ZERO_ENTRY),
            (8, Layer::Standard, "dpi-cycle", 0x42, ZERO_ENTRY),
            (1, Layer::EasyShift, "back", 0x40, [0x00, 0x04, 0x00]),
            (2, Layer::EasyShift, "forward", 0x40, [0x00, 0x05, 0x00]),
            (3, Layer::EasyShift, "next-track", 0x40, [0x02, 0xb5, 0x00]),
            (7, Layer::EasyShift, "stop", 0x40, [0x02, 0xb7, 0x00]),
            (
                4,
                Layer::EasyShift,
                "previous-track",
                0x40,
                [0x02, 0xb6, 0x00],
            ),
            (5, Layer::EasyShift, "play-pause", 0x40, [0x02, 0xcd, 0x00]),
            (3, Layer::Standard, "key:home", 0x40, [0x01, 0x00, 0x4a]),
            (1, Layer::EasyShift, "key:ctrl", 0x40, [0x01, 0x01, 0x00]),
            (
                4,
                Layer::EasyShift,
                "browser-search",
                0x40,
                [0x02, 0x21, 0x02],
            ),
            (4, Layer::Standard, "key:ctrl+t", 0x40, [0x01, 0x01, 0x17]),
            (
                5,
                Layer::EasyShift,
                "key:shift+alt+super+f5",
                0x40,
                [0x01, 0x0e, 0x3e],
            ),
            (3, Layer::Standard, "easy-aim-900", 0x56, ZERO_ENTRY),
            (7, Layer::Standard, "easy-wheel-volume", 0x4f, ZERO_ENTRY),
            (2, Layer::EasyShift, "profile:2", 0x49, ZERO_ENTRY),
            (
                7,
                Layer::Standard,
                "easy-aim:1650",
                0x40,
                [0x0c, 0x21, 0x00],
            ),
            (
                3,
                Layer::EasyShift,
                "easy-aim:5000",
                0x40,
                [0x0c, 0x64, 0x00],
            ),
        ];
        for (button, layer, action, code, entry) in cases {
            let (buttons, stored) = set(button, layer, action);
            let slot: usize = SLOTS[usize::from(button - 1)];
            assert_eq!(
                buttons.functions()[code_index(slot, layer)],
                code,
                "{action}"
            );
            assert_eq!(stored, entry, "{action}");
            assert_eq!(
                buttons.action(button, layer).expect("button").to_string(),
                action
            );
        }
    }

    #[test]
    fn easy_shift_button_has_no_easy_shift_function() {
        // Capture 53: button 3 Easy-Shift, its Easy-Shift function disabled.
        let (buttons, _) = set(3, Layer::Standard, "easy-shift");
        assert_eq!(buttons.functions()[2], 0x45);
        assert_eq!(buttons.functions()[EASY_SHIFT_CODES + 2], 0x00);
        assert_eq!(buttons.easy_shift[6..9], ZERO_ENTRY);
        assert!(!buttons.has_easy_shift_function(3).expect("button 3"));
        let mut buttons = buttons;
        assert!(
            buttons
                .set_action(3, Layer::EasyShift, ButtonAction::Disabled)
                .is_err()
        );
    }

    #[test]
    fn refuses_what_the_official_software_does_not_offer() {
        let refused: [(u8, Layer, &str); 9] = [
            (1, Layer::Standard, "menu"),
            (8, Layer::Standard, "click"),
            (8, Layer::Standard, "profile:1"),
            (8, Layer::EasyShift, "dpi-up"),
            (6, Layer::EasyShift, "click"),
            (4, Layer::Standard, "easy-shift"),
            (5, Layer::EasyShift, "easy-aim:800"),
            (4, Layer::Standard, "easy-wheel-dpi"),
            (2, Layer::EasyShift, "easy-shift"),
        ];
        for (button, layer, action) in refused {
            let mut buttons = start_buttons();
            let action: ButtonAction = action.parse().expect("valid action");
            assert!(
                buttons.set_action(button, layer, action).is_err(),
                "{button} {layer:?} {action}"
            );
            assert_eq!(buttons, start_buttons());
        }
        let mut buttons = start_buttons();
        assert!(
            buttons
                .set_action(0, Layer::Standard, ButtonAction::Disabled)
                .is_err()
        );
        assert!(
            buttons
                .set_action(9, Layer::Standard, ButtonAction::Disabled)
                .is_err()
        );
        assert!(
            buttons
                .set_action(
                    7,
                    Layer::Standard,
                    ButtonAction::Host(HostFunction::Swarm(2))
                )
                .is_err()
        );
    }

    #[test]
    fn host_functions_are_shown_and_kept() {
        // Captures 53/54: sleep, wake, log-off, shut down, lock, Open Swarm.
        let cases: [(u8, [u8; 3], &str); 6] = [
            (0x40, [0x05, 0x8e, 0x00], "system:sleep"),
            (0x40, [0x05, 0x8f, 0x00], "system:wake"),
            (0x40, [0x05, 0x74, 0x00], "system:shut-down"),
            (0x40, [0x0b, 0x25, 0x00], "swarm:lock"),
            (0x40, [0x0b, 0x02, 0x00], "swarm:open-swarm"),
            (0x40, [0x0b, 0x30, 0x00], "swarm:0x30"),
        ];
        for (code, entry, name) in cases {
            let action = ButtonAction::decode(code, entry);
            assert_eq!(action.to_string(), name);
            assert_eq!(action.encode(), (code, entry));
        }
        // Consumer usages beyond what konectl sets stay unknown.
        assert!(matches!(
            ButtonAction::decode(0x40, [0x02, 0x34, 0x12]),
            ButtonAction::Other { .. }
        ));
        let unknown = ButtonAction::decode(0x5f, ZERO_ENTRY);
        assert_eq!(unknown.to_string(), "unknown (5f 00 00 00)");
        assert_eq!(unknown.encode(), (0x5f, ZERO_ENTRY));
        assert!(matches!(
            ButtonAction::decode(0x40, [0x00, 0x09, 0x00]),
            ButtonAction::Other { .. }
        ));
    }

    #[test]
    fn parses_keys() {
        let key = |text: &str| text.parse::<Key>();
        assert_eq!(
            key("ctrl+c").expect("key"),
            Key {
                modifiers: 0x01,
                usage: 0x06
            }
        );
        assert_eq!(key("win+l").expect("key").to_string(), "super+l");
        assert_eq!(key("0x32").expect("key").to_string(), "0x32");
        assert_eq!(key("f13").expect("key").usage, 0x68);
        assert_eq!(key("kp-0").expect("key").usage, 0x62);
        assert_eq!(key("escape").expect("key").to_string(), "esc");
        for bad in [
            "",
            "ctrl+",
            "c+v",
            "ctrl+ctrl+c",
            "hyper+c",
            "0xe0",
            "0x02",
            "right-ctrl+c",
            "0x123",
            "0x+e",
        ] {
            assert!(key(bad).is_err(), "{bad:?}");
        }
        // Every name a key is shown with parses back to it.
        for usage in FIRST_KEY_USAGE..=0x73 {
            let key = Key {
                modifiers: 0,
                usage,
            };
            assert_eq!(key.to_string().parse::<Key>().expect("round trip"), key);
        }
    }

    #[test]
    fn parses_other_actions() {
        let parse = |text: &str| text.parse::<ButtonAction>();
        assert_eq!(
            parse("consumer:0x0223").expect("action"),
            ButtonAction::Consumer(0x0223)
        );
        assert_eq!(
            parse("consumer:0x00e2").expect("action").to_string(),
            "mute"
        );
        for bad in [
            "nothing",
            "consumer:223",
            "consumer:0x",
            "consumer:0x+e2",
            "easy-aim:fast",
            "easy-aim:+400",
            "profile:x",
            "profile:+2",
            "swarm:lock",
            "unknown",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        // Valid syntax, but values the mouse does not take.
        let mut buttons = start_buttons();
        for bad in [
            "easy-aim:50",
            "easy-aim:120",
            "easy-aim:19050",
            "profile:4",
            "consumer:0x0000",
        ] {
            let action = parse(bad).expect("syntax");
            assert!(
                buttons.set_action(7, Layer::Standard, action).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn large_easy_aim_dpi_uses_two_bytes() {
        let (_, entry) = set(7, Layer::Standard, "easy-aim:19000");
        assert_eq!(entry, [0x0c, 0x7c, 0x01]);
    }
}
