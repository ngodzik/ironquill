//! Colours as the interface's state holds them, whatever draws them.

/// A colour in red, green and blue. The terminal draws it as a 24-bit colour,
/// a window as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);
