//! Line wrapping for the transcript.
//!
//! Done here rather than by the paragraph widget, because scrolling needs the
//! number of wrapped lines before drawing, and the widget only exposes that
//! behind an unstable feature.

/// Splits `text` into lines of at most `width` characters, breaking at spaces
/// when it can and inside a word only when the word alone is too long.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in text.split('\n') {
        let mut current = String::new();
        let mut len = 0;
        for word in line.split(' ') {
            let word_len = word.chars().count();
            let needed = if len == 0 {
                word_len
            } else {
                len + 1 + word_len
            };
            if needed <= width {
                if len > 0 {
                    current.push(' ');
                }
                current.push_str(word);
                len = needed;
                continue;
            }
            if len > 0 {
                out.push(std::mem::take(&mut current));
            }
            // A word longer than the line is cut where the line ends.
            let mut chars = word.chars().peekable();
            len = 0;
            while chars.peek().is_some() {
                let chunk: String = chars.by_ref().take(width).collect();
                len = chunk.chars().count();
                if chars.peek().is_some() {
                    out.push(chunk);
                } else {
                    current = chunk;
                }
            }
        }
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaks_at_spaces() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
    }

    #[test]
    fn cuts_words_longer_than_the_line() {
        assert_eq!(wrap("abcdefghij x", 4), ["abcd", "efgh", "ij x"]);
    }

    #[test]
    fn keeps_newlines_and_empty_lines() {
        assert_eq!(wrap("a\n\nb", 10), ["a", "", "b"]);
    }

    #[test]
    fn counts_characters_not_bytes() {
        assert_eq!(wrap("éé éé", 5), ["éé éé"]);
    }
}
