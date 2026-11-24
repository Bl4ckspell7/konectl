//! `konectl udev`: install the udev rule that gives the logged-in user access to
//! the mouse's configuration channel, so konectl runs without sudo.

use std::io::ErrorKind;

use anyhow::Result;
use konectl::udev::{
    Access, Elevation, RELOAD, RULE, RULE_PATH, RuleState, TRIGGER, access, apply_script,
    install_script, other_rules, rule_state, uninstall_script,
};

/// Prints `script`, then runs it as root (through sudo unless already root)
/// with `stdin` piped in.
fn run_as_root(script: &[String], stdin: Option<&str>) -> Result<()> {
    let elevation: Elevation = Elevation::unless_root(Elevation::Sudo);
    println!(
        "running as root{}:",
        if elevation == Elevation::Sudo {
            " (sudo)"
        } else {
            ""
        }
    );
    for line in script {
        let note: &str = if line.starts_with("cat ") {
            "   # the rule, from konectl"
        } else {
            ""
        };
        println!("  {line}{note}");
    }

    match konectl::udev::run(script, stdin, elevation) {
        Ok(()) => Ok(()),
        Err(konectl::Error::CommandFailed { status, .. }) => {
            anyhow::bail!("the commands above failed ({status})")
        }
        // sudo or the shell could not be started: say how to do it by hand.
        Err(konectl::Error::Io { context, source })
            if matches!(
                source.kind(),
                ErrorKind::NotFound | ErrorKind::PermissionDenied
            ) =>
        {
            Err(anyhow::Error::new(source).context(format!(
                "{context}; install the rule as root yourself:\n  \
                 konectl udev rule > {RULE_PATH}\n  {RELOAD}\n  {TRIGGER}"
            )))
        }
        Err(error) => Err(error.into()),
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
    for path in other_rules() {
        println!("{:<8}also installed: {path}", "note");
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
