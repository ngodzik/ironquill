//! Keys as the interface's state reads them.
//!
//! The state never sees the window's own types: the window translates its
//! events into these, so that the state is tested without one. The names
//! follow Crossterm's, which the state was first written against.

use std::ops::BitOr;

/// Which key was pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// A character, as typed with the modifiers held.
    Char(char),
    /// Return.
    Enter,
    /// Escape.
    Esc,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Delete, forward.
    Delete,
    /// The up arrow.
    Up,
    /// The down arrow.
    Down,
    /// The left arrow.
    Left,
    /// The right arrow.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// A key nothing is bound to, such as a function key. It still counts
    /// as a key pressed: it clears a notice, as any key does.
    Other,
}

/// The modifiers held with a key or a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyModifiers(u8);

impl KeyModifiers {
    /// None held.
    pub const NONE: Self = Self(0);
    /// Shift.
    pub const SHIFT: Self = Self(1);
    /// Control.
    pub const CONTROL: Self = Self(1 << 1);
    /// Alt, or Option on a Mac.
    pub const ALT: Self = Self(1 << 2);

    /// Whether every modifier of `other` is held.
    #[must_use]
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for KeyModifiers {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// A key pressed, with the modifiers held. Releases and repeats never reach
/// the state: the backend drops them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    /// The key.
    pub code: KeyCode,
    /// What was held with it.
    pub modifiers: KeyModifiers,
}

impl KeyEvent {
    /// A key pressed with these modifiers.
    #[must_use]
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }
}

impl From<KeyCode> for KeyEvent {
    fn from(code: KeyCode) -> Self {
        Self::new(code, KeyModifiers::NONE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_combine_and_are_found() {
        let both = KeyModifiers::CONTROL | KeyModifiers::ALT;
        assert!(both.contains(KeyModifiers::CONTROL));
        assert!(both.contains(KeyModifiers::ALT));
        assert!(!both.contains(KeyModifiers::SHIFT));
        assert!(both.contains(KeyModifiers::NONE));
    }
}
