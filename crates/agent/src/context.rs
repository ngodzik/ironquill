//! The conversation as a text document a person can edit, and back.
//!
//! One block per message, under a `=== role` line. A tool call and its
//! result form a single `=== tool name {arguments}` block whose body is the
//! result, so that deleting the block removes both and the conversation
//! stays one a provider accepts.

use ironquill_core::{Message, ToolCall};

const HEADER: &str = "=== ";

/// The document's opening lines, which say how it works. Lines starting with
/// `#` before the first block are ignored when it is read back. They never
/// contain a block header, so that searching for one lands on a block.
const PREAMBLE: &str = "\
# The context the next request will be sent with. Edit it freely: delete
# passages, shorten tool results, add notes. :w applies it, :q! drops the edits.
# A tool block is a tool call with its result: deleting it removes both.
";

/// Writes `messages` as a document.
pub(crate) fn to_text(messages: &[Message]) -> String {
    let mut out = String::from(PREAMBLE);
    for message in messages {
        match message {
            Message::System(text) => block(&mut out, "system", text),
            Message::User(text) => block(&mut out, "user", text),
            Message::Assistant {
                content,
                tool_calls,
            } => {
                if let Some(text) = content.as_deref().filter(|t| !t.trim().is_empty()) {
                    block(&mut out, "assistant", text);
                }
                // Each call's result follows it in the conversation; it is
                // written with the call, and skipped when met on its own.
                for call in tool_calls {
                    let result = messages.iter().find_map(|m| match m {
                        Message::Tool { call_id, content } if *call_id == call.id => {
                            Some(content.as_str())
                        }
                        _ => None,
                    });
                    let title = format!("tool {} {}", call.name, call.arguments.trim());
                    block(&mut out, &title, result.unwrap_or(""));
                }
            }
            Message::Tool { .. } => {}
        }
    }
    out
}

fn block(out: &mut String, title: &str, body: &str) {
    out.push('\n');
    out.push_str(HEADER);
    out.push_str(title);
    out.push('\n');
    for line in body.lines() {
        // A body line that looks like a header is escaped, so that it reads
        // back as text.
        if line.starts_with(HEADER) || line.starts_with("\\===") {
            out.push('\\');
        }
        out.push_str(line);
        out.push('\n');
    }
}

/// Reads a document back into messages.
///
/// # Errors
///
/// A sentence for the person when a block has an unknown role, text comes
/// before the first block, or a tool block has no tool name.
pub(crate) fn from_text(text: &str) -> Result<Vec<Message>, String> {
    let mut blocks: Vec<(String, Vec<&str>)> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if let Some(title) = line.strip_prefix(HEADER) {
            blocks.push((title.trim().to_owned(), Vec::new()));
        } else if let Some((_, body)) = blocks.last_mut() {
            body.push(
                line.strip_prefix('\\')
                    .filter(|l| l.starts_with("==="))
                    .unwrap_or(line),
            );
        } else if !line.trim().is_empty() && !line.starts_with('#') {
            return Err(format!(
                "Line {}: text before the first === block",
                number + 1
            ));
        }
    }

    let mut messages = Vec::new();
    let mut calls = 0;
    for (title, body) in blocks {
        // Trailing blank lines are the separation between blocks, not content.
        let body = body.join("\n").trim_end().to_owned();
        let (role, rest) = title.split_once(' ').unwrap_or((title.as_str(), ""));
        match role {
            "system" => messages.push(Message::System(body)),
            "user" => messages.push(Message::User(body)),
            "assistant" => messages.push(Message::Assistant {
                content: Some(body),
                tool_calls: Vec::new(),
            }),
            "tool" => {
                let (name, arguments) = rest.trim().split_once(' ').unwrap_or((rest.trim(), "{}"));
                if name.is_empty() {
                    return Err(
                        "A tool block needs a tool name: === tool <name> {arguments}".into(),
                    );
                }
                calls += 1;
                let call = ToolCall {
                    id: format!("context_{calls}"),
                    name: name.to_owned(),
                    arguments: arguments.trim().to_owned(),
                };
                // The call joins the assistant message right before it when
                // there is one, as it was; otherwise it gets its own.
                match messages.last_mut() {
                    Some(Message::Assistant { tool_calls, .. }) => tool_calls.push(call.clone()),
                    _ => messages.push(Message::Assistant {
                        content: None,
                        tool_calls: vec![call.clone()],
                    }),
                }
                messages.push(Message::Tool {
                    call_id: call.id,
                    content: body,
                });
            }
            other => {
                return Err(format!(
                    "Unknown block === {other}: use system, user, assistant or tool"
                ));
            }
        }
    }
    Ok(order_results(messages))
}

/// Puts each tool result right after the assistant message that called it,
/// the order providers require, whatever order the blocks were read in.
fn order_results(messages: Vec<Message>) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    let mut results: Vec<Message> = Vec::new();
    for message in messages {
        match message {
            Message::Tool { .. } => results.push(message),
            other => {
                if !results.is_empty() && !matches!(other, Message::Assistant { content: None, .. })
                {
                    out.append(&mut results);
                }
                out.push(other);
            }
        }
    }
    out.append(&mut results);
    // An assistant message holding several calls followed by their results is
    // already in order; an assistant message with calls and then text cannot
    // occur, since text blocks come before the calls they precede.
    out
}

/// A rough size in tokens, four characters each, enough to see what an edit
/// saves.
pub(crate) fn approx_tokens(messages: &[Message]) -> u64 {
    let chars: usize = messages
        .iter()
        .map(|m| match m {
            Message::System(t) | Message::User(t) => t.len(),
            Message::Assistant {
                content,
                tool_calls,
            } => {
                content.as_deref().map_or(0, str::len)
                    + tool_calls
                        .iter()
                        .map(|c| c.name.len() + c.arguments.len())
                        .sum::<usize>()
            }
            Message::Tool { content, .. } => content.len(),
        })
        .sum();
    (chars / 4) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation() -> Vec<Message> {
        vec![
            Message::system("rules"),
            Message::user("read a.rs"),
            Message::Assistant {
                content: Some("Reading it.".into()),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.rs"}"#.into(),
                }],
            },
            Message::Tool {
                call_id: "c1".into(),
                content: "fn a() {}\n=== tricky".into(),
            },
            Message::Assistant {
                content: Some("It defines a.".into()),
                tool_calls: vec![],
            },
        ]
    }

    #[test]
    fn a_conversation_reads_back_as_it_was_written() {
        let text = to_text(&conversation());
        assert!(text.contains("=== tool read_file {\"path\":\"a.rs\"}"));
        assert!(!PREAMBLE.contains("==="));
        let back = from_text(&text).unwrap();
        assert_eq!(back.len(), 5);
        assert_eq!(back[1], Message::user("read a.rs"));
        let Message::Assistant { tool_calls, .. } = &back[2] else {
            panic!("the call should stay with its assistant message");
        };
        let Message::Tool { call_id, content } = &back[3] else {
            panic!("the result should follow its call");
        };
        assert_eq!(*call_id, tool_calls[0].id);
        assert_eq!(content, "fn a() {}\n=== tricky");
    }

    #[test]
    fn deleting_a_tool_block_removes_the_call_and_its_result() {
        let text = to_text(&conversation());
        let start = text.find("\n=== tool").unwrap() + 1;
        let end = text[start..].find("\n=== assistant").unwrap() + start + 1;
        let edited = format!("{}{}", &text[..start], &text[end..]);
        let back = from_text(&edited).unwrap();
        assert!(back.iter().all(|m| !matches!(m, Message::Tool { .. })));
        assert!(back.iter().all(
            |m| !matches!(m, Message::Assistant { tool_calls, .. } if !tool_calls.is_empty())
        ));
    }

    #[test]
    fn notes_and_mistakes() {
        assert!(from_text("stray text\n=== user\nhi").is_err());
        assert!(from_text("=== robot\nhi").is_err());
        assert!(from_text("=== tool\nresult").is_err());
        let back = from_text("# a comment\n=== user\nhi\n\n=== user\nnote: keep it short").unwrap();
        assert_eq!(
            back,
            [Message::user("hi"), Message::user("note: keep it short")]
        );
    }

    #[test]
    fn size_shrinks_with_the_text() {
        let full = conversation();
        let small = vec![Message::user("x")];
        assert!(approx_tokens(&small) < approx_tokens(&full));
    }
}
