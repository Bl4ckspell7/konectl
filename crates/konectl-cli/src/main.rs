//! `konectl` — command-line front end of the konectl library.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use konectl::protocol::{REPORT_LEN, Report, VENDOR_ID};
use konectl::{HidTransport, KoneProAir, Manager, Rgb};

/// Configure a ROCCAT Kone Pro Air mouse (unofficial).
#[derive(Debug, Parser)]
#[command(name = "konectl", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List connected Kone Pro Air devices.
    List,
    /// Show the battery level.
    Battery,
    /// Show the wireless signal strength.
    Signal,
    /// Set the LED colours until the mouse goes to sleep (not saved on the mouse).
    Led {
        /// Left LED colour, e.g. ff8800 or '#ff8800'.
        left: Rgb,
        /// Right LED colour; defaults to the left colour.
        right: Option<Rgb>,
        /// Brightness in percent.
        #[arg(short, long, default_value_t = 100, value_parser = clap::value_parser!(u8).range(0..=100))]
        brightness: u8,
    },
    /// Send a raw report and print the replies (debugging).
    ///
    /// Meant for protocol work. Unknown commands can change or break the mouse
    /// configuration — only send what the official software was seen sending.
    Raw {
        /// Hex bytes, e.g. "90 0a" or 900a; zero-padded to 64 bytes.
        #[arg(required = true)]
        bytes: Vec<String>,
    },
}

fn main() -> Result<()> {
    let cli: Cli = Cli::parse();
    let manager: Manager = Manager::new().context("failed to initialise HID access")?;

    match cli.command {
        Command::List => list(&manager)?,
        Command::Battery => {
            let battery = open(&manager)?.battery()?;
            match battery.percent() {
                Some(percent) => println!("{percent}%"),
                None => println!("unknown (raw {})", battery.raw),
            }
        }
        Command::Signal => {
            let signal = open(&manager)?.signal()?;
            if signal.radio_enabled() {
                println!("{}% (raw {})", signal.percent(), signal.raw);
            } else {
                println!("radio off");
            }
        }
        Command::Led {
            left,
            right,
            brightness,
        } => {
            let right: Rgb = right.unwrap_or(left);
            open(&manager)?.set_leds(left.scaled(brightness), right.scaled(brightness))?;
        }
        Command::Raw { bytes } => {
            let report: Report = parse_hex_report(&bytes)?;
            let replies: Vec<Report> = open(&manager)?.raw(&report)?;
            if replies.is_empty() {
                println!("no reply");
            }
            for reply in &replies {
                println!("{}", format_hex(reply));
            }
        }
    }
    Ok(())
}

fn list(manager: &Manager) -> Result<()> {
    let devices = manager.devices();
    if devices.is_empty() {
        bail!(konectl::Error::NotFound);
    }
    for device in &devices {
        println!(
            "{:<18}  {:04x}:{:04x}  iface {}  {}  serial {}",
            device.mode,
            VENDOR_ID,
            device.mode.product_id(),
            device.mode.interface(),
            device.path.to_string_lossy(),
            device.serial.as_deref().unwrap_or("-"),
        );
    }
    Ok(())
}

fn open(manager: &Manager) -> Result<KoneProAir<HidTransport>> {
    match manager.open_first() {
        Ok(mouse) => Ok(mouse),
        Err(error @ konectl::Error::Hid(_)) => Err(anyhow::Error::new(error).context(
            "could not open the mouse (no access to /dev/hidraw*? install udev/70-konectl.rules or use sudo)",
        )),
        Err(error) => Err(error.into()),
    }
}

/// Parses whitespace-separated hex tokens into a zero-padded report.
///
/// Each token holds whole bytes (`0a`, `900a`), so `1 2` is rejected instead
/// of being read as `12`.
fn parse_hex_report(parts: &[String]) -> Result<Report> {
    let mut bytes: Vec<u8> = Vec::new();
    for token in parts.iter().flat_map(|part| part.split_whitespace()) {
        if !token.len().is_multiple_of(2) || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("invalid hex token {token:?}: write whole bytes like 0a or 900a");
        }
        for start in (0..token.len()).step_by(2) {
            bytes.push(u8::from_str_radix(&token[start..start + 2], 16)?);
        }
    }
    if bytes.is_empty() {
        bail!("no bytes given");
    }
    if bytes.len() > REPORT_LEN {
        bail!(
            "report has {} bytes, the maximum is {REPORT_LEN}",
            bytes.len()
        );
    }
    let mut report: Report = [0; REPORT_LEN];
    report[..bytes.len()].copy_from_slice(&bytes);
    Ok(report)
}

fn format_hex(report: &Report) -> String {
    report
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<String>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(parts: &[&str]) -> Result<Report> {
        let parts: Vec<String> = parts.iter().map(|part| (*part).to_owned()).collect();
        parse_hex_report(&parts)
    }

    #[test]
    fn parses_spaced_and_joined_hex() {
        let report = parse(&["90 0a", "FF"]).expect("valid hex");
        assert_eq!(report[..3], [0x90, 0x0a, 0xff]);
        assert!(report[3..].iter().all(|&byte| byte == 0));
        assert_eq!(parse(&["900aff"]).expect("valid hex"), report);
    }

    #[test]
    fn rejects_bad_hex() {
        assert!(parse(&["90 0"]).is_err());
        assert!(parse(&["zz"]).is_err());
        assert!(parse(&["+1"]).is_err());
        assert!(parse(&[&"00".repeat(REPORT_LEN + 1)]).is_err());
    }

    #[test]
    fn single_digits_are_not_merged() {
        assert!(parse(&["1", "2"]).is_err());
        assert!(parse(&["9 0 0 a"]).is_err());
    }

    #[test]
    fn rejects_empty_input() {
        assert!(parse(&[""]).is_err());
        assert!(parse(&["  "]).is_err());
    }

    #[test]
    fn accepts_full_length_report() {
        assert!(parse(&[&"ab".repeat(REPORT_LEN)]).is_ok());
    }

    #[test]
    fn formats_hex() {
        let mut report: Report = [0; REPORT_LEN];
        report[0] = 0x90;
        report[1] = 0x0a;
        assert!(format_hex(&report).starts_with("90 0a 00"));
    }

    #[test]
    fn cli_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
