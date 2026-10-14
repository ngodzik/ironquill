//! Colours as the interface's state holds them, whatever draws them.

/// A colour in red, green and blue, as the window draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);
