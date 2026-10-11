//! Keys as the window reports them, turned into the state's own.
//!
//! The state reads every key itself, as in the terminal: no egui widget
//! takes the keyboard, so that the modes, the leader and the window prefix
//! work the same in both.

use bevy::input::keyboard::Key;
use universe_ui::input::{KeyCode, KeyEvent, KeyModifiers};

/// What a key pressed asks of the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pressed {
    /// A key for the state.
    Key(KeyEvent),
    /// Paste the clipboard: Ctrl-V, Ctrl-Shift-V, Cmd-V or Shift-Insert, as
    /// in most applications. The terminal pastes by itself; a window must
    /// ask.
    Paste,
}

/// The modifiers held, as the window knows them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Held {
    pub(crate) shift: bool,
    pub(crate) control: bool,
    /// The left Alt only: the right one is AltGr on many layouts, which types
    /// `{`, `@` or `#` and must reach the state as the character alone.
    pub(crate) alt: bool,
    /// Cmd on macOS, the Super key elsewhere.
    pub(crate) command: bool,
}

impl Held {
    fn modifiers(self) -> KeyModifiers {
        let mut held = KeyModifiers::NONE;
        if self.shift {
            held = held | KeyModifiers::SHIFT;
        }
        if self.control {
            held = held | KeyModifiers::CONTROL;
        }
        if self.alt {
            held = held | KeyModifiers::ALT;
        }
        held
    }
}

/// Whether the modifiers egui reports with C ask for a copy: Cmd on
/// macOS; Ctrl-Shift elsewhere, Ctrl-C alone being the state's.
pub(crate) fn copies(modifiers: bevy_egui::egui::Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.mac_cmd
    } else {
        modifiers.ctrl && modifiers.shift
    }
}

/// What pressing `key` with `held` asks for, or `None` for a key alone that
/// means nothing, such as Shift: the terminal never reports those either.
pub(crate) fn translate(key: &Key, held: Held) -> Option<Pressed> {
    let code = match key {
        Key::Character(text) => {
            let c = text.chars().next()?;
            if (held.control || held.command) && c.eq_ignore_ascii_case(&'v') {
                return Some(Pressed::Paste);
            }
            // Cmd with a letter is the system's (Cmd-C copies, through
            // egui), never the letter typed; so is the copy off macOS.
            if held.command || (held.control && held.shift && c.eq_ignore_ascii_case(&'c')) {
                return None;
            }
            // With Control held, the state reads the letter as typed without
            // Shift, as the terminal reports Ctrl-O.
            if held.control {
                KeyCode::Char(c.to_ascii_lowercase())
            } else {
                KeyCode::Char(c)
            }
        }
        Key::Space => KeyCode::Char(' '),
        Key::Enter => KeyCode::Enter,
        Key::Escape => KeyCode::Esc,
        Key::Tab => KeyCode::Tab,
        Key::Backspace => KeyCode::Backspace,
        Key::Delete => KeyCode::Delete,
        Key::ArrowUp => KeyCode::Up,
        Key::ArrowDown => KeyCode::Down,
        Key::ArrowLeft => KeyCode::Left,
        Key::ArrowRight => KeyCode::Right,
        Key::Home => KeyCode::Home,
        Key::End => KeyCode::End,
        Key::PageUp => KeyCode::PageUp,
        Key::PageDown => KeyCode::PageDown,
        Key::Insert if held.shift => return Some(Pressed::Paste),
        Key::Shift
        | Key::Control
        | Key::Alt
        | Key::AltGraph
        | Key::Super
        | Key::Meta
        | Key::Hyper
        | Key::CapsLock
        | Key::NumLock
        | Key::Fn
        | Key::FnLock => return None,
        _ => KeyCode::Other,
    };
    // Shift is in the character already; the terminal does not report it
    // beside one either.
    let held = match code {
        KeyCode::Char(_) => Held {
            shift: false,
            ..held
        },
        _ => held,
    };
    Some(Pressed::Key(KeyEvent::new(code, held.modifiers())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn character(c: &str) -> Key {
        Key::Character(c.into())
    }

    #[test]
    fn characters_arrive_as_typed_and_control_letters_lowercase() {
        let shift = Held {
            shift: true,
            ..Held::default()
        };
        assert_eq!(
            translate(&character("A"), shift),
            Some(Pressed::Key(KeyCode::Char('A').into()))
        );
        let control = Held {
            control: true,
            ..Held::default()
        };
        assert_eq!(
            translate(&character("O"), control),
            Some(Pressed::Key(KeyEvent::new(
                KeyCode::Char('o'),
                KeyModifiers::CONTROL
            )))
        );
    }

    #[test]
    fn named_keys_map_and_modifiers_alone_do_not() {
        assert_eq!(
            translate(&Key::Escape, Held::default()),
            Some(Pressed::Key(KeyCode::Esc.into()))
        );
        assert_eq!(
            translate(&Key::Space, Held::default()),
            Some(Pressed::Key(KeyCode::Char(' ').into()))
        );
        assert_eq!(translate(&Key::Shift, Held::default()), None);
        assert_eq!(
            translate(&Key::F5, Held::default()),
            Some(Pressed::Key(KeyCode::Other.into()))
        );
    }

    #[test]
    fn the_clipboard_is_pasted_the_usual_ways() {
        let control = Held {
            control: true,
            ..Held::default()
        };
        assert_eq!(translate(&character("v"), control), Some(Pressed::Paste));
        let command = Held {
            command: true,
            ..Held::default()
        };
        assert_eq!(translate(&character("v"), command), Some(Pressed::Paste));
        // Any other Cmd-letter is the system's, not typed.
        assert_eq!(translate(&character("c"), command), None);
        assert_eq!(translate(&character("q"), command), None);
        let shift = Held {
            shift: true,
            ..Held::default()
        };
        assert_eq!(translate(&Key::Insert, shift), Some(Pressed::Paste));
    }
}
