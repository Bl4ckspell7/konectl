//! The udev rule that gives the logged-in user access to the mouse's
//! configuration channel, so konectl runs without root.
//!
//! This module checks the installed rule and builds the shell commands that
//! install or remove it; [`run`] runs them as root through sudo or pkexec.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::device::Manager;
use crate::error::{Error, Result};

/// The rule that ships with this version of konectl (`udev/70-konectl.rules`).
pub const RULE: &str = include_str!("../70-konectl.rules");
/// Where konectl installs the rule.
pub const RULE_PATH: &str = "/etc/udev/rules.d/70-konectl.rules";
/// Written first and then moved into place, so udev never reads half a file.
/// udev ignores it: it does not end in `.rules`.
const TEMPORARY_PATH: &str = "/etc/udev/rules.d/.70-konectl.rules.tmp";
/// Other places udev reads rules from, e.g. a distribution package.
pub const OTHER_RULE_PATHS: [&str; 2] = [
    "/usr/lib/udev/rules.d/70-konectl.rules",
    "/run/udev/rules.d/70-konectl.rules",
];

/// Reloads the rules.
pub const RELOAD: &str = "udevadm control --reload";
/// Applies the rules to the connected hidraw devices.
pub const TRIGGER: &str = "udevadm trigger --settle --subsystem-match=hidraw";

/// State of [`RULE_PATH`] compared to [`RULE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleState {
    Installed,
    /// Installed, but the rules differ (e.g. from an older konectl).
    Outdated,
    Missing,
    /// A symbolic link, usually a mask (`→ /dev/null`); konectl leaves it alone.
    Masked,
}

/// Lines that matter to udev: not blank, not comments.
fn rule_lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// State of an installed rule with this content; comment changes do not count.
pub fn content_state(installed: &str) -> RuleState {
    if rule_lines(installed) == rule_lines(RULE) {
        RuleState::Installed
    } else {
        RuleState::Outdated
    }
}

/// State of the rule at [`RULE_PATH`].
pub fn rule_state() -> Result<RuleState> {
    let metadata = match fs::symlink_metadata(RULE_PATH) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(RuleState::Missing),
        Err(error) => return Err(Error::io(format!("could not read {RULE_PATH}"), error)),
    };
    if metadata.file_type().is_symlink() {
        return Ok(RuleState::Masked);
    }
    let content: String = fs::read_to_string(RULE_PATH)
        .map_err(|error| Error::io(format!("could not read {RULE_PATH}"), error))?;
    Ok(content_state(&content))
}

/// Copies of the rule outside `/etc` that exist, e.g. from a distribution
/// package.
pub fn other_rules() -> Vec<&'static str> {
    OTHER_RULE_PATHS
        .into_iter()
        .filter(|path| fs::symlink_metadata(path).is_ok())
        .collect()
}

/// Shell lines that write the rule (from stdin: [`RULE`]) atomically and
/// apply it.
pub fn install_script() -> Vec<String> {
    vec![
        format!("cat > {TEMPORARY_PATH}"),
        format!("chmod 0644 {TEMPORARY_PATH}"),
        format!("mv -f {TEMPORARY_PATH} {RULE_PATH}"),
        RELOAD.to_owned(),
        TRIGGER.to_owned(),
    ]
}

/// Shell lines that apply the installed rule again.
pub fn apply_script() -> Vec<String> {
    vec![RELOAD.to_owned(), TRIGGER.to_owned()]
}

/// Shell lines that remove the rule and apply the change.
pub fn uninstall_script() -> Vec<String> {
    vec![
        format!("rm -f {RULE_PATH}"),
        RELOAD.to_owned(),
        TRIGGER.to_owned(),
    ]
}

/// How [`run`] becomes root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevation {
    /// Already root.
    None,
    /// `sudo` (asks for the password on the terminal).
    Sudo,
    /// `pkexec` (polkit asks for the password in a dialog).
    Pkexec,
}

impl Elevation {
    /// `None` when running as root, otherwise `fallback`.
    pub fn unless_root(fallback: Self) -> Self {
        if is_root() { Self::None } else { fallback }
    }
}

/// The program and arguments that run `script` in one shell as root (so
/// there is a single password prompt).
pub fn invocation(script: &[String], elevation: Elevation) -> Vec<String> {
    let mut args: Vec<String> = match elevation {
        Elevation::None => vec!["sh".to_owned()],
        Elevation::Sudo => vec!["sudo".to_owned(), "sh".to_owned()],
        // pkexec runs programs with a minimal PATH: give the full path.
        Elevation::Pkexec => vec!["pkexec".to_owned(), "/bin/sh".to_owned()],
    };
    args.push("-c".to_owned());
    args.push(format!("set -eu\n{}", script.join("\n")));
    args
}

/// `true` when running as root (effective user id 0).
pub fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout.trim_ascii() == b"0")
}

/// Runs `script` as root with `stdin` piped in.
///
/// Fails with [`Error::Io`] when the program cannot be started (its source
/// is then e.g. [`ErrorKind::NotFound`]) or does not take `stdin`, and with
/// [`Error::CommandFailed`] when it exits unsuccessfully (pkexec: 126 when
/// the password dialog was dismissed, 127 when not authorized).
pub fn run(script: &[String], stdin: Option<&str>, elevation: Elevation) -> Result<()> {
    let args: Vec<String> = invocation(script, elevation);
    // What ran, without `-c` and the script.
    let label: String = args[..args.len() - 2].join(" ");
    let mut command = Command::new(&args[0]);
    command.args(&args[1..]);
    command.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = command
        .spawn()
        .map_err(|error| Error::io(format!("could not run {}", args[0]), error))?;
    if let Some(input) = stdin {
        let written = match child.stdin.take() {
            Some(mut pipe) => pipe.write_all(input.as_bytes()),
            None => Err(ErrorKind::BrokenPipe.into()),
        };
        if let Err(error) = written {
            // Do not leave the shell behind, e.g. waiting for its input.
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::io(
                format!("could not pass the input to {label}"),
                error,
            ));
        }
    }
    let status = child
        .wait()
        .map_err(|error| Error::io(format!("{label} did not finish"), error))?;
    if !status.success() {
        return Err(Error::CommandFailed {
            command: label,
            status,
        });
    }
    Ok(())
}

/// Whether the logged-in user can open the mouse's configuration channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// Accessible: connection mode and device path.
    Accessible(String),
    /// Not accessible: device path.
    NotAccessible(String),
    /// Running as root, which says nothing about the user's access.
    NotChecked,
    NoDevice,
    Unknown(String),
}

/// Checks whether the first connected mouse can be opened; nothing is sent
/// to the mouse.
pub fn access() -> Access {
    if is_root() {
        return Access::NotChecked;
    }
    let manager: Manager = match Manager::new() {
        Ok(manager) => manager,
        Err(error) => return Access::Unknown(error.to_string()),
    };
    let Some(device) = manager.devices().into_iter().next() else {
        return Access::NoDevice;
    };
    let path: String = device.path.to_string_lossy().into_owned();
    match OpenOptions::new()
        .read(true)
        .write(true)
        .open(Path::new(&path))
    {
        Ok(_) => Access::Accessible(format!("{}, {path}", device.mode)),
        Err(error) if error.kind() == ErrorKind::PermissionDenied => Access::NotAccessible(path),
        Err(error) => Access::Unknown(format!("{path}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_rule_covers_both_connections() {
        assert!(RULE.contains(
            r#"ATTRS{idProduct}=="2c8e", ENV{ID_USB_INTERFACE_NUM}=="02", TAG+="uaccess""#
        ));
        assert!(RULE.contains(
            r#"ATTRS{idProduct}=="2c92", ENV{ID_USB_INTERFACE_NUM}=="01", TAG+="uaccess""#
        ));
    }

    #[test]
    fn comments_do_not_make_a_rule_outdated() {
        assert_eq!(content_state(RULE), RuleState::Installed);
        let recommented: String = format!("# installed by hand\n\n{}", RULE.replace("# ", "#  "));
        assert_eq!(content_state(&recommented), RuleState::Installed);
        let changed: String = RULE.replace("2c92", "2c93");
        assert_eq!(content_state(&changed), RuleState::Outdated);
        assert_eq!(content_state(""), RuleState::Outdated);
    }

    #[test]
    fn install_writes_atomically_and_applies() {
        assert_eq!(
            install_script(),
            [
                "cat > /etc/udev/rules.d/.70-konectl.rules.tmp",
                "chmod 0644 /etc/udev/rules.d/.70-konectl.rules.tmp",
                "mv -f /etc/udev/rules.d/.70-konectl.rules.tmp /etc/udev/rules.d/70-konectl.rules",
                "udevadm control --reload",
                "udevadm trigger --settle --subsystem-match=hidraw",
            ]
        );
        assert_eq!(
            uninstall_script()[0],
            "rm -f /etc/udev/rules.d/70-konectl.rules"
        );
        assert_eq!(apply_script().len(), 2);
    }

    #[test]
    fn one_shell_through_sudo_or_pkexec_unless_root() {
        let script: Vec<String> = apply_script();
        let through_sudo: Vec<String> = invocation(&script, Elevation::Sudo);
        assert_eq!(through_sudo[..3], ["sudo", "sh", "-c"]);
        assert_eq!(
            through_sudo[3],
            "set -eu\nudevadm control --reload\nudevadm trigger --settle --subsystem-match=hidraw"
        );
        assert_eq!(
            invocation(&script, Elevation::Pkexec)[..3],
            ["pkexec", "/bin/sh", "-c"]
        );
        assert_eq!(invocation(&script, Elevation::None)[..2], ["sh", "-c"]);
    }

    #[test]
    fn run_passes_stdin_and_reports_failures() {
        let check: Vec<String> = vec![r#"test "$(cat)" = rule"#.to_owned()];
        assert!(run(&check, Some("rule"), Elevation::None).is_ok());
        match run(&check, Some("other"), Elevation::None) {
            Err(Error::CommandFailed { command, status }) => {
                assert_eq!(command, "sh");
                assert_eq!(status.code(), Some(1));
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
        // `set -eu`: a failing line stops the script.
        let stops: Vec<String> = vec!["false".to_owned(), "exit 3".to_owned()];
        assert!(matches!(
            run(&stops, None, Elevation::None),
            Err(Error::CommandFailed { status, .. }) if status.code() == Some(1)
        ));
    }
}
