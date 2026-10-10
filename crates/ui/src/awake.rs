//! Keeping the machine from idle sleep while ticks run: a sleeping machine
//! sends no tick, and the caches go cold anyway. Held exactly while ticks
//! can run, released the moment they cannot.
//!
//! On macOS, `caffeinate -i -w <pid>`: idle sleep only (the display may
//! sleep, closing the lid still sleeps the machine), tied to ironquill's
//! process so that even a crash cannot leave the machine held awake. On
//! Linux, `systemd-inhibit` on idle. Elsewhere, nothing.

use std::process::{Child, Command, Stdio};

/// The program that holds the machine awake here, with its arguments, if
/// there is one.
fn program() -> Option<(&'static str, Vec<String>)> {
    if cfg!(target_os = "macos") {
        Some((
            "caffeinate",
            vec!["-i".into(), "-w".into(), std::process::id().to_string()],
        ))
    } else if cfg!(target_os = "linux") {
        Some((
            "systemd-inhibit",
            vec![
                "--what=idle".into(),
                "--who=ironquill".into(),
                "--why=keeping prompt caches warm".into(),
                "--mode=block".into(),
                "sleep".into(),
                "infinity".into(),
            ],
        ))
    } else {
        None
    }
}

/// The machine held awake, while it is.
#[derive(Debug, Default)]
pub(crate) struct Awake {
    child: Option<Child>,
    /// The program could not be started: it is not tried again, and the
    /// person was told once.
    failed: bool,
}

impl Awake {
    /// Holds the machine awake when `wanted`, and releases it otherwise.
    /// Returns what to tell the person, the first time it cannot.
    pub(crate) fn hold(&mut self, wanted: bool) -> Option<String> {
        if !wanted {
            self.release();
            return None;
        }
        // Still held: a program that ended on its own is started again.
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(None) => return None,
                _ => self.child = None,
            }
        }
        if self.failed {
            return None;
        }
        let (name, args) = program()?;
        match Command::new(name)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => {
                self.child = Some(child);
                None
            }
            Err(_) => {
                self.failed = true;
                Some(format!(
                    "{name} is not installed: the machine may sleep while ticks are on, and the \
                     caches go cold"
                ))
            }
        }
    }

    /// Whether the machine is held awake now.
    pub(crate) fn is_held(&self) -> bool {
        self.child.is_some()
    }

    fn release(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_only_while_wanted_and_released_at_once() {
        let mut awake = Awake::default();
        assert_eq!(awake.hold(false), None);
        assert!(!awake.is_held());
        let told = awake.hold(true);
        // Where the program exists, it is held; where it does not, the
        // person is told once, and not again.
        if awake.is_held() {
            assert_eq!(told, None);
            assert_eq!(awake.hold(true), None);
            assert_eq!(awake.hold(false), None);
            assert!(!awake.is_held());
        } else if program().is_some() {
            assert!(told.is_some());
            assert_eq!(awake.hold(true), None);
        }
    }
}
