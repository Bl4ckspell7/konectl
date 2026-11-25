//! "Allow access": installs the udev rule that lets the logged-in user open the
//! mouse, with pkexec (polkit asks for the password). Inside the Flatpak
//! pkexec cannot reach the host, so the app shows the commands instead.

use std::io::ErrorKind;
use std::path::Path;

use konectl::udev::{self, Elevation, RELOAD, RULE, RULE_PATH, RuleState, TRIGGER};

/// What happened when the user asked for access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The rule is installed and applied.
    Allowed,
    /// The user closed the password dialog.
    Dismissed,
}

/// Whether this runs inside a Flatpak sandbox.
pub fn in_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

/// Shell commands that install the rule by hand, for a terminal on the host.
/// Without the rule's comments, which do not count when konectl compares it.
pub fn manual_commands() -> String {
    let rule: String = RULE
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .collect::<Vec<&str>>()
        .join("\n");
    format!("sudo tee {RULE_PATH} >/dev/null <<'EOF'\n{rule}\nEOF\nsudo {RELOAD}\nsudo {TRIGGER}\n")
}

/// Installs the rule (or applies it again if it is installed already).
/// Blocks until the password dialog is answered; errors are for the user.
pub fn allow() -> Result<Outcome, String> {
    let state: RuleState = udev::rule_state().map_err(|error| error.to_string())?;
    let (script, stdin): (Vec<String>, Option<&str>) = match state {
        RuleState::Masked => {
            return Err(format!(
                "{RULE_PATH} is a symbolic link (probably a mask), so konectl leaves it alone. \
                 Remove it first."
            ));
        }
        RuleState::Installed => (udev::apply_script(), None),
        RuleState::Missing | RuleState::Outdated => (udev::install_script(), Some(RULE)),
    };
    let elevation: Elevation = Elevation::unless_root(Elevation::Pkexec);
    log::info!("udev rule {state:?}: running {script:?} ({elevation:?})");
    let outcome = outcome(udev::run(&script, stdin, elevation));
    // A failure is logged once, where the worker reports it.
    if let Ok(result) = &outcome {
        log::info!("udev rule: {result:?}");
    }
    outcome
}

/// What a pkexec run means for the user.
fn outcome(result: konectl::Result<()>) -> Result<Outcome, String> {
    match result {
        Ok(()) => Ok(Outcome::Allowed),
        // pkexec: 126 = the dialog was dismissed, 127 = not authorized (also
        // what the shell returns for a missing command).
        Err(konectl::Error::CommandFailed { status, .. }) => match status.code() {
            Some(126) => Ok(Outcome::Dismissed),
            Some(127) => Err("Access was not granted (not authorized, or a command is \
                              missing). Install the rule in a terminal instead."
                .to_owned()),
            _ => Err(format!(
                "Installing the rule failed ({status}). Install it in a terminal instead."
            )),
        },
        Err(konectl::Error::Io { source, .. }) if source.kind() == ErrorKind::NotFound => {
            Err("pkexec is not installed. Install the rule in a terminal instead.".to_owned())
        }
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;

    use super::*;

    fn failed(code: i32) -> konectl::Result<()> {
        Err(konectl::Error::CommandFailed {
            command: "pkexec /bin/sh".to_owned(),
            status: ExitStatus::from_raw(code << 8),
        })
    }

    #[test]
    fn pkexec_exit_codes() {
        assert_eq!(outcome(Ok(())), Ok(Outcome::Allowed));
        assert_eq!(outcome(failed(126)), Ok(Outcome::Dismissed));
        assert!(outcome(failed(127)).is_err_and(|message| message.contains("not authorized")));
        assert!(outcome(failed(1)).is_err_and(|message| message.contains("exit status: 1")));
    }

    #[test]
    fn manual_commands_write_the_rule_and_apply_it() {
        let commands: String = manual_commands();
        assert!(commands.starts_with("sudo tee /etc/udev/rules.d/70-konectl.rules >/dev/null"));
        assert!(commands.ends_with(&format!("\nEOF\nsudo {RELOAD}\nsudo {TRIGGER}\n")));
        // What the heredoc writes counts as the installed rule.
        let written: &str = commands
            .split_once("<<'EOF'\n")
            .and_then(|(_, rest)| rest.split_once("\nEOF\n"))
            .map(|(rule, _)| rule)
            .expect("heredoc");
        assert!(!written.contains('#'));
        assert_eq!(udev::content_state(written), RuleState::Installed);
    }
}
