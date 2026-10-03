//! The conversation as a text document a person can edit, and back.
//!
//! One block per message, under a `=== role` line. A tool call and its
//! result form a single `=== tool name {arguments}` block whose body is the
//! result, so that deleting the block removes both and the conversation
//! stays one a provider accepts.

use ironquill_core::{Message, ToolCall};

const HEADER: &str = "=== ";

/// An assistant turn being read back: its text, and each call with its result.
type Turn = (Option<String>, Vec<(ToolCall, String)>);

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

    // Messages are rebuilt as turns: an assistant turn is its text and every
    // tool block that follows it, before any other block. However blocks were
    // deleted or moved, each turn becomes one assistant message holding all
    // its calls, followed by their results: the shape providers require.
    let mut messages = Vec::new();
    let mut turn: Option<Turn> = None;
    let mut calls = 0;
    for (title, body) in blocks {
        // Trailing blank lines are the separation between blocks, not content.
        let body = body.join("\n").trim_end().to_owned();
        let (role, rest) = title.split_once(' ').unwrap_or((title.as_str(), ""));
        match role {
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
                turn.get_or_insert_with(|| (None, Vec::new()))
                    .1
                    .push((call, body));
            }
            "assistant" => {
                flush(&mut messages, turn.take());
                turn = Some((Some(body), Vec::new()));
            }
            "system" | "user" => {
                flush(&mut messages, turn.take());
                messages.push(if role == "system" {
                    Message::System(body)
                } else {
                    Message::User(body)
                });
            }
            other => {
                return Err(format!(
                    "Unknown block === {other}: use system, user, assistant or tool"
                ));
            }
        }
    }
    flush(&mut messages, turn);
    Ok(messages)
}

/// Writes one assistant turn: its text and calls, then each call's result.
fn flush(messages: &mut Vec<Message>, turn: Option<Turn>) {
    let Some((content, exchanges)) = turn else {
        return;
    };
    let content = content.filter(|c| !c.trim().is_empty());
    if content.is_none() && exchanges.is_empty() {
        return;
    }
    messages.push(Message::Assistant {
        content,
        tool_calls: exchanges.iter().map(|(call, _)| call.clone()).collect(),
    });
    for (call, result) in exchanges {
        messages.push(Message::Tool {
            call_id: call.id,
            content: result,
        });
    }
}

/// The conversation in the shape providers accept: each assistant message
/// keeps only the calls that have a result, each result follows its call at
/// once and only once, a result without its call is dropped, and an assistant
/// message left with neither text nor calls goes too.
pub(crate) fn sanitize(messages: Vec<Message>) -> Vec<Message> {
    let mut results: std::collections::HashMap<String, String> = messages
        .iter()
        .filter_map(|m| match m {
            Message::Tool { call_id, content } => Some((call_id.clone(), content.clone())),
            _ => None,
        })
        .collect();
    let mut out = Vec::new();
    for message in messages {
        match message {
            Message::Tool { .. } => {}
            Message::Assistant {
                content,
                tool_calls,
            } => {
                let answered: Vec<(ToolCall, String)> = tool_calls
                    .into_iter()
                    .filter_map(|call| {
                        let result = results.remove(&call.id)?;
                        Some((call, result))
                    })
                    .collect();
                flush(&mut out, Some((content, answered)));
            }
            other => out.push(other),
        }
    }
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

/// Results of earlier tool calls longer than this are dropped by a
/// compaction; shorter ones cost little and say a lot.
const COMPACT_LONGER_THAN: usize = 400;

/// Replaces the content of every long tool result but the last `keep` with
/// a line saying what it was, so that a long task stops resending files it
/// read long ago. The model can call the tool again. Messages stay where
/// they are, so the conversation stays valid. Returns how many results were
/// dropped.
pub(crate) fn compact(messages: &mut [Message], keep: usize) -> usize {
    let calls: std::collections::HashMap<String, (String, String)> = messages
        .iter()
        .flat_map(|m| match m {
            Message::Assistant { tool_calls, .. } => tool_calls.as_slice(),
            _ => &[],
        })
        .map(|c| (c.id.clone(), (c.name.clone(), c.arguments.clone())))
        .collect();
    let results: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m, Message::Tool { .. }))
        .map(|(i, _)| i)
        .collect();
    let old = results.len().saturating_sub(keep);
    let mut dropped = 0;
    for &i in &results[..old] {
        let Message::Tool { call_id, content } = &mut messages[i] else {
            continue;
        };
        if content.len() <= COMPACT_LONGER_THAN {
            continue;
        }
        let (name, arguments) = calls
            .get(call_id)
            .cloned()
            .unwrap_or_else(|| ("a tool".into(), String::new()));
        *content = format!(
            "[Earlier result of {name} {arguments}, {} lines, dropped to save space. Call the \
             tool again if you need it.]",
            content.lines().count()
        );
        dropped += 1;
    }
    dropped
}

#[cfg(test)]
pub(crate) mod tests {
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

    /// What providers require: each assistant message with calls is followed
    /// at once by one result per call, and no result stands alone.
    pub(crate) fn valid(messages: &[Message]) -> bool {
        let mut i = 0;
        while i < messages.len() {
            match &messages[i] {
                Message::Assistant { tool_calls, .. } if !tool_calls.is_empty() => {
                    for call in tool_calls {
                        i += 1;
                        match messages.get(i) {
                            Some(Message::Tool { call_id, .. }) if *call_id == call.id => {}
                            _ => return false,
                        }
                    }
                }
                Message::Tool { .. } => return false,
                _ => {}
            }
            i += 1;
        }
        true
    }

    pub(crate) fn two_tools() -> Vec<Message> {
        let call = |id: &str, path: &str| ToolCall {
            id: id.into(),
            name: "read_file".into(),
            arguments: format!(r#"{{"path":"{path}"}}"#),
        };
        vec![
            Message::system("rules"),
            Message::user("read both"),
            Message::Assistant {
                content: Some("Reading them.".into()),
                tool_calls: vec![call("c1", "a.rs"), call("c2", "b.rs")],
            },
            Message::Tool {
                call_id: "c1".into(),
                content: "A".into(),
            },
            Message::Tool {
                call_id: "c2".into(),
                content: "B".into(),
            },
            Message::Assistant {
                content: Some("Done.".into()),
                tool_calls: vec![],
            },
            Message::user("next"),
        ]
    }

    #[test]
    fn several_calls_in_one_reply_read_back_as_one_valid_turn() {
        let back = from_text(&to_text(&two_tools())).unwrap();
        assert!(valid(&back), "{back:#?}");
        let Message::Assistant { tool_calls, .. } = &back[2] else {
            panic!("the turn should start with its assistant message");
        };
        assert_eq!(tool_calls.len(), 2);
    }

    #[test]
    fn deleting_any_block_leaves_a_valid_conversation() {
        let text = to_text(&two_tools());
        // Block boundaries: the line index of each header.
        let lines: Vec<&str> = text.lines().collect();
        let headers: Vec<usize> = (0..lines.len())
            .filter(|i| lines[*i].starts_with(HEADER))
            .collect();
        for (n, &start) in headers.iter().enumerate() {
            let end = headers.get(n + 1).copied().unwrap_or(lines.len());
            let edited: Vec<&str> = lines[..start]
                .iter()
                .chain(&lines[end..])
                .copied()
                .collect();
            let back = from_text(&edited.join("\n")).unwrap();
            assert!(valid(&back), "after deleting {}: {back:#?}", lines[start]);
        }
    }

    #[test]
    fn a_broken_conversation_is_put_back_in_shape() {
        let mut broken = two_tools();
        // Results moved away from their call, as the earlier reading did.
        let second = broken.remove(4);
        broken.insert(5, second);
        assert!(!valid(&broken));
        let fixed = sanitize(broken);
        assert!(valid(&fixed), "{fixed:#?}");

        // A call left without its result, as a stopped request leaves it.
        let mut stopped = two_tools();
        stopped.truncate(4);
        let fixed = sanitize(stopped);
        assert!(valid(&fixed), "{fixed:#?}");
        assert!(
            !fixed
                .iter()
                .any(|m| matches!(m, Message::Tool { call_id, .. } if call_id == "c2"))
        );
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
    fn compaction_drops_old_long_results_and_keeps_the_conversation_valid() {
        let read = |id: &str, path: &str| Message::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: "read_file".into(),
                arguments: format!("{{\"path\":\"{path}\"}}"),
            }],
        };
        let result = |id: &str, text: String| Message::Tool {
            call_id: id.into(),
            content: text,
        };
        let long = "line\n".repeat(200);
        let mut messages = vec![
            Message::system("rules"),
            Message::user("fix it"),
            read("1", "a.py"),
            result("1", long.clone()),
            read("2", "b.py"),
            result("2", "short".into()),
            read("3", "c.py"),
            result("3", long.clone()),
        ];
        let before = approx_tokens(&messages);

        assert_eq!(compact(&mut messages, 1), 1);
        assert!(valid(&messages));
        assert_eq!(
            messages[3],
            Message::Tool {
                call_id: "1".into(),
                content: "[Earlier result of read_file {\"path\":\"a.py\"}, 200 lines, dropped \
                          to save space. Call the tool again if you need it.]"
                    .into(),
            }
        );
        // Short results and the latest one stay.
        assert_eq!(messages[5], result("2", "short".into()));
        assert_eq!(messages[7], result("3", long));
        assert!(approx_tokens(&messages) < before);
        // Nothing left to drop the second time.
        assert_eq!(compact(&mut messages, 1), 0);
    }

    #[test]
    fn size_shrinks_with_the_text() {
        let full = conversation();
        let small = vec![Message::user("x")];
        assert!(approx_tokens(&small) < approx_tokens(&full));
    }
}
