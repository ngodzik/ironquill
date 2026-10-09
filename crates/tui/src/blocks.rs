//! The fenced blocks of a reply, as text: what a copy mark copies and what
//! `/apply` applies. Drawing them is the view's business.

/// The ```diff blocks of `text`, numbered: by the number after `diff`, as
/// `/address` asks for, else in order from 1.
pub(crate) fn diff_blocks(text: &str) -> Vec<(usize, String)> {
    let mut blocks = Vec::new();
    // The block being read: whether a diff, its number, its lines.
    let mut current: Option<(bool, Option<usize>, Vec<&str>)> = None;
    let mut order = 0;
    for line in text.lines() {
        if let Some(info) = line.trim_start().strip_prefix("```") {
            match current.take() {
                Some((true, number, lines)) => {
                    order += 1;
                    blocks.push((number.unwrap_or(order), lines.join("\n") + "\n"));
                }
                Some((false, _, _)) => {}
                None => {
                    let mut words = info.split_whitespace();
                    let diff = words.next() == Some("diff");
                    let number = words.next().and_then(|n| n.parse().ok());
                    current = Some((diff, number, Vec::new()));
                }
            }
        } else if let Some((_, _, lines)) = current.as_mut() {
            lines.push(line);
        }
    }
    blocks
}

/// The code blocks of `text`, as written, in order: what each copy mark
/// copies.
pub(crate) fn code_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            match current.take() {
                Some(lines) => blocks.push(lines.join("\n")),
                None => current = Some(Vec::new()),
            }
        } else if let Some(lines) = current.as_mut() {
            lines.push(line);
        }
    }
    // A block left open still copies what it has.
    if let Some(lines) = current {
        blocks.push(lines.join("\n"));
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_blocks_are_numbered_as_labelled_or_in_order() {
        let text = "1.\n```diff 3\n-a\n+b\n```\n```text\nreply\n```\n```diff\n-c\n+d\n```";
        assert_eq!(
            diff_blocks(text),
            [(3, "-a\n+b\n".to_owned()), (2, "-c\n+d\n".to_owned())]
        );
    }
}
