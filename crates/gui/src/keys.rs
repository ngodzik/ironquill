//! Keys as the window reports them, turned into the state's own.
//!
//! The state reads every key itself, as in the terminal: no egui widget
//! takes the keyboard, so that the modes, the leader and the window prefix
//! work the same in both.

use bevy::input::keyboard::Key;
use ironquill_ui::input::{KeyCode, KeyEvent, KeyModifiers};

/// What a key pressed asks of the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pressed {
    /// A key for the state.
    Key(KeyEvent),
    /// Paste the clipboard: Ctrl-V, Ctrl-Shift-V or Shift-Insert, as in
    /// most applications. The terminal pastes by itself; a window must ask.
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

/// What pressing `key` with `held` asks for, or `None` for a key alone that
/// means nothing, such as Shift: the terminal never reports those either.
pub(crate) fn translate(key: &Key, held: Held) -> Option<Pressed> {
    let code = match key {
        Key::Character(text) => {
            let c = text.chars().next()?;
            if held.control && c.eq_ignore_ascii_case(&'v') {
                return Some(Pressed::Paste);
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
        let shift = Held {
            shift: true,
            ..Held::default()
        };
        assert_eq!(translate(&Key::Insert, shift), Some(Pressed::Paste));
    }
}
