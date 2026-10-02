//! The system clipboard, for Vim's `+` and `*` registers.
//!
//! Through the platform's own command when there is one: `pbcopy` on macOS,
//! `wl-copy` on Wayland, `xclip` or `xsel` on X11. Without any, copying falls
//! back to the OSC 52 escape sequence, which most terminals turn into a
//! clipboard write, over SSH too. Pasting has no such fallback.

use std::io::Write;
use std::process::{Command, Stdio};

/// Commands that read text on stdin into the clipboard, most specific first.
const COPY: [(&str, &[&str]); 4] = [
    ("pbcopy", &[]),
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
];

/// Commands that print the clipboard on stdout.
const PASTE: [(&str, &[&str]); 4] = [
    ("pbpaste", &[]),
    ("wl-paste", &["--no-newline"]),
    ("xclip", &["-selection", "clipboard", "-o"]),
    ("xsel", &["--clipboard", "--output"]),
];

/// Puts `text` on the system clipboard. Returns how, for the status message.
pub(crate) fn copy(text: &str) -> Result<&'static str, String> {
    for (program, args) in COPY {
        let Ok(mut child) = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let written = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
        if child.wait().is_ok_and(|s| s.success()) && written {
            return Ok(program);
        }
    }
    osc52(text).map(|()| "the terminal")
}

/// Reads the system clipboard.
pub(crate) fn paste() -> Result<String, String> {
    for (program, args) in PASTE {
        if let Ok(output) = Command::new(program)
            .args(args)
            .stderr(Stdio::null())
            .output()
            && output.status.success()
        {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
    }
    Err("No clipboard command found (pbpaste, wl-paste, xclip or xsel)".into())
}

/// Asks the terminal to set its clipboard. The sequence draws nothing, so it
/// is safe to send while the interface owns the screen.
fn osc52(text: &str) -> Result<(), String> {
    let mut out = std::io::stdout();
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))
        .and_then(|()| out.flush())
        .map_err(|e| format!("Cannot reach the clipboard: {e}"))
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(TABLE[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("héllo".as_bytes()), "aMOpbGxv");
    }
}
