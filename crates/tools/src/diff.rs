/// One line of a change, as shown to the person.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DiffLine {
    /// An unchanged line next to the change, for orientation.
    Context(String),
    /// A line that was removed.
    Removed(String),
    /// A line that was added.
    Added(String),
}

/// How many unchanged lines frame a change.
const CONTEXT: usize = 1;

/// The lines that differ between `old` and `new`, framed by a little context.
///
/// # Examples
///
/// ```
/// use ironquill_tools::{DiffLine, line_diff};
///
/// assert_eq!(
///     line_diff("a = 1", "a = 2"),
///     [DiffLine::Removed("a = 1".into()), DiffLine::Added("a = 2".into())]
/// );
/// ```
///
/// Not a minimal diff: the common head and tail are trimmed and everything in
/// between is shown as removed then added. Model edits are local, so the middle
/// is small, and this is exact about what changed without a diff algorithm.
pub fn line_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();

    let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();

    // Nothing changed: no context either, or the person would see a diff of
    // a file that is the same as before.
    if head == old.len() && head == new.len() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for line in &old[head.saturating_sub(CONTEXT)..head] {
        out.push(DiffLine::Context((*line).to_owned()));
    }
    for line in &old[head..old.len() - tail] {
        out.push(DiffLine::Removed((*line).to_owned()));
    }
    for line in &new[head..new.len() - tail] {
        out.push(DiffLine::Added((*line).to_owned()));
    }
    let after = old.len() - tail;
    for line in &old[after..(after + CONTEXT).min(old.len())] {
        out.push(DiffLine::Context((*line).to_owned()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_only_the_changed_middle_with_context() {
        let old = "a\nb\nc\nd\ne";
        let new = "a\nb\nX\nY\nd\ne";
        assert_eq!(
            line_diff(old, new),
            [
                DiffLine::Context("b".into()),
                DiffLine::Removed("c".into()),
                DiffLine::Added("X".into()),
                DiffLine::Added("Y".into()),
                DiffLine::Context("d".into()),
            ]
        );
    }

    #[test]
    fn a_new_file_is_all_added() {
        assert_eq!(
            line_diff("", "one\ntwo"),
            [DiffLine::Added("one".into()), DiffLine::Added("two".into())]
        );
    }

    #[test]
    fn identical_text_has_no_diff() {
        assert!(line_diff("same\n", "same\n").is_empty());
    }
}
