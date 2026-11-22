//! `konectl udev`: install the udev rule that gives the logged-in user access to
//! the mouse's configuration channel, so konectl runs without sudo.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use konectl::Manager;

/// The rule that ships with this version of konectl (`udev/70-konectl.rules`).
pub(crate) const RULE: &str = include_str!("../70-konectl.rules");
/// Where konectl installs the rule.
pub(crate) const RULE_PATH: &str = "/etc/udev/rules.d/70-konectl.rules";
/// Written first and then moved into place, so udev never reads half a file.
/// udev ignores it: it does not end in `.rules`.
const TEMPORARY_PATH: &str = "/etc/udev/rules.d/.70-konectl.rules.tmp";
/// Other places udev reads rules from, e.g. a distribution package.
const OTHER_RULE_PATHS: [&str; 2] = [
    "/usr/lib/udev/rules.d/70-konectl.rules",
    "/run/udev/rules.d/70-konectl.rules",
];

const RELOAD: &str = "udevadm control --reload";
const TRIGGER: &str = "udevadm trigger --settle --subsystem-match=hidraw";

/// State of [`RULE_PATH`] compared to [`RULE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuleState {
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
pub(crate) fn content_state(installed: &str) -> RuleState {
    if rule_lines(installed) == rule_lines(RULE) {
        RuleState::Installed
    } else {
        RuleState::Outdated
    }
}

fn rule_state() -> Result<RuleState> {
    let metadata = match fs::symlink_metadata(RULE_PATH) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(RuleState::Missing),
        Err(error) => return Err(error).with_context(|| format!("could not read {RULE_PATH}")),
    };
    if metadata.file_type().is_symlink() {
        return Ok(RuleState::Masked);
    }
    let content: String =
        fs::read_to_string(RULE_PATH).with_context(|| format!("could not read {RULE_PATH}"))?;
    Ok(content_state(&content))
}

/// Shell lines that write the rule (from stdin) atomically and apply it.
pub(crate) fn install_script() -> Vec<String> {
    vec![
        format!("cat > {TEMPORARY_PATH}"),
        format!("chmod 0644 {TEMPORARY_PATH}"),
        format!("mv -f {TEMPORARY_PATH} {RULE_PATH}"),
        RELOAD.to_owned(),
        TRIGGER.to_owned(),
    ]
}

/// Shell lines that apply the installed rule again.
pub(crate) fn apply_script() -> Vec<String> {
    vec![RELOAD.to_owned(), TRIGGER.to_owned()]
}

pub(crate) fn uninstall_script() -> Vec<String> {
    vec![
        format!("rm -f {RULE_PATH}"),
        RELOAD.to_owned(),
        TRIGGER.to_owned(),
    ]
}

/// The program and arguments that run `script` in one shell, through sudo
/// unless already root (so there is a single password prompt).
pub(crate) fn invocation(script: &[String], as_root: bool) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if !as_root {
        args.push("sudo".to_owned());
    }
    args.extend(["sh".to_owned(), "-c".to_owned()]);
    args.push(format!("set -eu\n{}", script.join("\n")));
    args
}

/// `true` when running as root (effective user id 0).
fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout.trim_ascii() == b"0")
}

/// Prints `script`, then runs it as root with `stdin` piped in.
fn run_as_root(script: &[String], stdin: Option<&str>) -> Result<()> {
    let as_root: bool = is_root();
    println!("running as root{}:", if as_root { "" } else { " (sudo)" });
    for line in script {
        let note: &str = if line.starts_with("cat ") {
            "   # the rule, from konectl"
        } else {
            ""
        };
        println!("  {line}{note}");
    }

    let args: Vec<String> = invocation(script, as_root);
    let mut command = Command::new(&args[0]);
    command.args(&args[1..]);
    command.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = command.spawn().with_context(|| {
        format!(
            "could not run {}; install the rule as root yourself:\n  \
             konectl udev rule > {RULE_PATH}\n  {RELOAD}\n  {TRIGGER}",
            args[0]
        )
    })?;
    if let Some(input) = stdin {
        let mut pipe = child.stdin.take().context("no stdin for the shell")?;
        pipe.write_all(input.as_bytes())
            .context("could not pass the rule to the shell")?;
    }
    let status = child.wait().context("the shell did not finish")?;
    if !status.success() {
        bail!("the commands above failed ({status})");
    }
    Ok(())
}

/// Whether the logged-in user can open the mouse's configuration channel.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Access {
    Accessible(String),
    NotAccessible(String),
    /// Running as root, which says nothing about the user's access.
    NotChecked,
    NoDevice,
    Unknown(String),
}

fn access() -> Access {
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
    // Opening the node only checks the permissions; nothing is sent to the mouse.
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

/// Prints the access state; the hint for "not accessible" depends on the rule.
fn print_access(access: &Access, rule: RuleState) {
    let text: String = match access {
        Access::Accessible(device) => format!("accessible ({device})"),
        Access::NotAccessible(path) => match rule {
            RuleState::Installed => format!(
                "not accessible ({path}); replug the dongle or cable, or log out and in \
                 (the rule only applies to the active local session)"
            ),
            RuleState::Missing | RuleState::Outdated => {
                format!("not accessible ({path}); run `konectl udev install`")
            }
            RuleState::Masked => format!("not accessible ({path}); the rule is masked"),
        },
        Access::NotChecked => {
            "not checked (running as root; run `konectl udev status` without sudo)".to_owned()
        }
        Access::NoDevice => "no Kone Pro Air connected".to_owned(),
        Access::Unknown(reason) => format!("unknown ({reason})"),
    };
    println!("{:<8}{text}", "mouse");
}

/// Copies of the rule outside /etc, e.g. from a distribution package.
fn print_other_rules() {
    for path in OTHER_RULE_PATHS {
        if fs::symlink_metadata(path).is_ok() {
            println!("{:<8}also installed: {path}", "note");
        }
    }
}

fn masked_error() -> anyhow::Error {
    anyhow::anyhow!(
        "{RULE_PATH} is a symbolic link (probably a mask); konectl leaves it alone. \
         Remove it first if konectl should install the rule."
    )
}

pub(crate) fn install() -> Result<()> {
    match rule_state()? {
        RuleState::Masked => return Err(masked_error()),
        RuleState::Installed => {
            let current: Access = access();
            if matches!(current, Access::NotAccessible(_)) {
                println!("the rule is installed; applying it again");
                run_as_root(&apply_script(), None)?;
            } else {
                println!("the udev rule is already installed: {RULE_PATH}");
                print_access(&current, RuleState::Installed);
                return Ok(());
            }
        }
        RuleState::Missing | RuleState::Outdated => {
            run_as_root(&install_script(), Some(RULE))?;
            println!("installed {RULE_PATH}");
        }
    }
    print_access(&access(), RuleState::Installed);
    Ok(())
}

pub(crate) fn uninstall() -> Result<()> {
    match rule_state()? {
        RuleState::Masked => return Err(masked_error()),
        RuleState::Missing => println!("the udev rule is not installed: {RULE_PATH}"),
        RuleState::Installed | RuleState::Outdated => {
            run_as_root(&uninstall_script(), None)?;
            println!("removed {RULE_PATH}");
            println!(
                "access granted earlier stays until the dongle or cable is replugged \
                 or you log out"
            );
        }
    }
    print_other_rules();
    Ok(())
}

pub(crate) fn status() -> Result<()> {
    let rule: RuleState = rule_state()?;
    let state: &str = match rule {
        RuleState::Installed => "installed",
        RuleState::Outdated => {
            "outdated (differs from this konectl version; run `konectl udev install`)"
        }
        RuleState::Missing => "not installed (run `konectl udev install`)",
        RuleState::Masked => "a symbolic link (probably a mask); konectl leaves it alone",
    };
    println!("{:<8}{state}: {RULE_PATH}", "rule");
    print_other_rules();
    print_access(&access(), rule);
    Ok(())
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
    fn one_shell_through_sudo_unless_root() {
        let script: Vec<String> = apply_script();
        let through_sudo: Vec<String> = invocation(&script, false);
        assert_eq!(through_sudo[..3], ["sudo", "sh", "-c"]);
        assert_eq!(
            through_sudo[3],
            "set -eu\nudevadm control --reload\nudevadm trigger --settle --subsystem-match=hidraw"
        );
        assert_eq!(invocation(&script, true)[..2], ["sh", "-c"]);
    }
}
