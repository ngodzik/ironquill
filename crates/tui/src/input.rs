//! Keys, clicks and screen areas as the interface's state reads them.
//!
//! The state never sees a backend's own types: the terminal loop translates
//! Crossterm's events into these, and a graphical window will translate its
//! own. The names follow Crossterm's so that the code reading them reads as
//! it did.

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

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// The primary button.
    Left,
    /// The secondary button.
    Right,
    /// The wheel, pressed.
    Middle,
}

/// What the mouse did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseEventKind {
    /// A button went down.
    Down(MouseButton),
    /// The wheel turned up.
    ScrollUp,
    /// The wheel turned down.
    ScrollDown,
    /// Anything else: a release, a drag, a move. Nothing reads them yet.
    Other,
}

/// What the mouse did, and where, in cells from the top left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MouseEvent {
    /// What it did.
    pub kind: MouseEventKind,
    /// The cell's column.
    pub column: u16,
    /// The cell's row.
    pub row: u16,
    /// What was held meanwhile.
    pub modifiers: KeyModifiers,
}

/// A cell on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Position {
    /// Its column.
    pub x: u16,
    /// Its row.
    pub y: u16,
}

impl Position {
    /// The cell at this column and row.
    #[must_use]
    pub fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

/// A rectangle of cells on screen: where a pane was drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    /// The column of its left edge.
    pub x: u16,
    /// The row of its top edge.
    pub y: u16,
    /// How many columns it spans.
    pub width: u16,
    /// How many rows it spans.
    pub height: u16,
}

impl Rect {
    /// The rectangle at this corner, of this size.
    #[must_use]
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Whether the cell is inside: the right and bottom edges are outside,
    /// as with Ratatui's rectangles.
    #[must_use]
    pub fn contains(self, at: Position) -> bool {
        // Widened, so that a rectangle at the screen's far edge cannot wrap.
        let (x, y) = (u32::from(at.x), u32::from(at.y));
        x >= u32::from(self.x)
            && x < u32::from(self.x) + u32::from(self.width)
            && y >= u32::from(self.y)
            && y < u32::from(self.y) + u32::from(self.height)
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

    #[test]
    fn a_rectangle_holds_its_cells_and_not_its_far_edges() {
        let r = Rect::new(2, 3, 4, 5);
        assert!(r.contains(Position::new(2, 3)));
        assert!(r.contains(Position::new(5, 7)));
        assert!(!r.contains(Position::new(6, 3)));
        assert!(!r.contains(Position::new(2, 8)));
        assert!(!r.contains(Position::new(1, 3)));
        let edge = Rect::new(u16::MAX - 1, 0, 10, 1);
        assert!(edge.contains(Position::new(u16::MAX, 0)));
    }
}
