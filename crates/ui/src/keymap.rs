//! Which key does what, in which mode and which pane.
//!
//! Every binding lives in [`action`]. Making the bindings configurable later
//! means replacing this one function with a table read from a file; nothing
//! else in the interface knows about keys.
//!
//! Movement is on the arrow keys everywhere. Vim's spirit is kept in the modes,
//! the `,` leader and the `Ctrl-W` window prefix, not in `hjkl`.

use crate::input::{KeyCode, KeyEvent, KeyModifiers};

/// The editing mode, as in Vim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Moving around the panes.
    Normal,
    /// Typing a message.
    Insert,
    /// Typing a `:` command.
    Command,
    /// Typing a search of the codebase's maps, which keeps what it names
    /// as it is typed.
    Search,
}

/// The pane that receives movement keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The file tree on the left.
    Tree,
    /// The open file.
    File,
    /// The conversation.
    Chat,
    /// The running Docker containers, under the other panes.
    Docker,
    /// What a model of the team did on a task handed to it.
    SubAgent,
}

/// The first key of a two-key binding, waiting for the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    /// `,` as in Vim's leader key.
    Leader,
    /// `Ctrl-W`, Vim's window prefix.
    Window,
}

/// What a key asks for, independent of which key it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Switch mode.
    Enter(Mode),
    /// Remember the first key of a two-key binding.
    Wait(Pending),
    /// Type a character.
    Insert(char),
    /// Delete the character before the cursor.
    Backspace,
    /// Delete the word before the cursor.
    DeleteWord,
    /// Clear the line.
    ClearLine,
    /// Move the cursor one character left.
    Left,
    /// Move the cursor one character right.
    Right,
    /// Move the cursor to the start of the line.
    Home,
    /// Move the cursor to the end of the line.
    End,
    /// Send the message, or run the command.
    Submit,
    /// Move in the focused pane by this many lines; positive is down.
    Move(i32),
    /// Show the message sent this many messages away from the one shown,
    /// back when negative; past the latest, the draft.
    Recall(i32),
    /// Move by half a screen; `true` is down.
    HalfPage(bool),
    /// Jump to the top of the focused pane.
    Top,
    /// Jump to the bottom of the focused pane.
    Bottom,
    /// Open the selected tree row: expand a directory, show a file.
    Open,
    /// Collapse the selected directory, or go to its parent.
    Collapse,
    /// Show or hide the file tree.
    ToggleTree,
    /// Go to the file tree, opening it if it is hidden.
    FocusTree,
    /// Give the whole screen to the conversation, or give the other panes back.
    Zoom,
    /// Show or hide what the last model of the team handed a task did.
    ToggleSubAgent,
    /// Unfold or fold the selected reply.
    Fold,
    /// Show or hide the Docker containers pane.
    ToggleDocker,
    /// Go to the message box, ready to type, from wherever the focus is.
    FocusInput,
    /// Open the model picker.
    PickModel,
    /// Show or hide the list of every shortcut.
    ShowKeys,
    /// Close the open file and return to the conversation.
    ShowChat,
    /// Close the focused pane.
    ClosePane,
    /// Focus the next pane, left to right, wrapping around.
    FocusNext,
    /// Focus the pane on the left.
    FocusLeft,
    /// Focus the pane on the right.
    FocusRight,
    /// Stop the running request.
    Cancel,
    /// Leave ironquill.
    Quit,
    /// Copy the selected reply, or command and its output.
    CopySelected,
    /// Show or hide the usage pane.
    ToggleUsage,
    /// The usage pane's next window: an hour, six, a day.
    NextUsageWindow,
    /// Let what is behind the window show through, or make it opaque again.
    ToggleSeeThrough,
    /// Show the codebase's plan, its API, its universe, then the panes again.
    NextMap,
    /// Search the codebase's maps, while one is shown.
    Search,
}

/// Every shortcut, as Ctrl-S lists them. Kept next to the bindings above so
/// that a change to one shows the other needing the same change.
pub const SHORTCUTS: &[(&str, &[(&str, &str)])] = &[
    (
        "Anywhere",
        &[
            ("Ctrl-Q", "back to typing a message (Ctrl-G too)"),
            (
                "Ctrl-E",
                "the models: type to search, Enter answers, Space team, ← → effort, Delete off the list",
            ),
            ("Ctrl-A", "go to the file tree, opening it if hidden"),
            ("Ctrl-B", "show or hide the file tree"),
            (
                "Ctrl-Z",
                "conversation full screen, and back to the panes as they were",
            ),
            (
                "Ctrl-T",
                "show or hide the sub-agent pane: what the model handed a task did",
            ),
            ("Ctrl-K", "show or hide the Docker containers"),
            ("Ctrl-O", "show or hide the usage pane (/usage)"),
            ("Ctrl-P", "the usage pane's window: 1h, 6h, 24h"),
            (
                "Ctrl-M",
                "the window see-through, as a terminal can be, or opaque (--gui only)",
            ),
            (
                "Ctrl-N",
                "the codebase's plan, its API, its universe of files, then back (--gui only)",
            ),
            (
                "Ctrl-F",
                "search the plan, the API and the universe: Enter keeps the search, Esc clears it (--gui only)",
            ),
            ("Ctrl-S", "this list"),
            ("Ctrl-C", "stop the request; twice to quit"),
            ("Tab", "next pane"),
        ],
    ),
    (
        "Typing a message",
        &[
            ("Enter", "send"),
            ("/", "a command: /help lists them"),
            ("Esc", "normal mode"),
            ("Ctrl-W / Ctrl-U", "delete a word / the line"),
            ("Up Down", "the messages sent before, as in a shell"),
            ("PgUp PgDn", "scroll the conversation"),
        ],
    ),
    (
        "Normal mode",
        &[
            ("i", "type a message"),
            (":", "a command"),
            ("?", "this list"),
            (",n ,d ,m ,i", "tree, Docker, model, type"),
            (",c", "close the file, back to the conversation"),
            ("Ctrl-W Left/Right", "pane on the left / right"),
            (
                "Up Down Home End",
                "move in the pane; in the conversation, select a reply",
            ),
            (
                "Enter Space",
                "unfold or fold the selected reply or command",
            ),
            ("y", "copy the selected reply, or command and its output"),
        ],
    ),
    (
        "File tree",
        &[
            (
                "M ? A D",
                "modified, new, added, deleted since the last commit",
            ),
            ("Up Down", "move"),
            ("Right or Enter", "open a file, unfold a folder"),
            ("Left", "fold the folder, or go to its parent"),
            ("q", "hide the tree"),
        ],
    ),
    (
        "Open file (Vim)",
        &[
            ("i a o O I A", "insert; Esc stops"),
            ("v V", "visual mode, by character / by line"),
            ("x dd D yy p P", "delete, yank, put"),
            ("\"+y \"+p", "copy to / paste from the system clipboard"),
            ("u Ctrl-R", "undo, redo"),
            (":w :q :q! :wq :42", "write, close, discard, go to line"),
            (":s/a/b/g", "substitute; with %, '<,'> or 2,5"),
            ("/ n N", "search, next, previous"),
            (
                "zo zc za zR zM",
                "open, close, toggle a block; open all, close all (in /context); in /review \
                 and /work, zo opens unchanged lines, zR shows the whole file, zM only its changes",
            ),
            (
                "gd Ctrl-click",
                "go to where the name under the cursor is defined, as its language server says \
                 (pyright, typescript-language-server, rust-analyzer), else git grep; several: \
                 choose with ↑ ↓ Enter",
            ),
            ("gr", "list where the name under the cursor is used"),
            (
                "Ctrl-O Alt-← Alt-→",
                "back to where gd left from, and forward again",
            ),
            ("Alt-↑ Alt-↓", "to the previous / next change"),
            (
                "Enter Space",
                "open or close the block under the cursor (in /context)",
            ),
            ("gg G w b 0 ^ $", "move"),
            (
                "green / yellow / red",
                "line added / changed / removed since the last commit",
            ),
        ],
    ),
    (
        "Lists (resume, model, Docker)",
        &[
            ("Up Down", "choose"),
            ("Enter", "open"),
            ("Esc or q", "close"),
        ],
    ),
];

/// The action bound to `key` in `mode` with `focus`, after `pending` if a
/// two-key binding was started.
pub fn action(mode: Mode, focus: Focus, pending: Option<Pending>, key: KeyEvent) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // Bindings that work everywhere.
    match key.code {
        KeyCode::Char('c') if ctrl => return Some(Action::Cancel),
        KeyCode::Char('d') if ctrl && mode != Mode::Normal => return Some(Action::Quit),
        // As the sidebar shortcut of common editors, so that the tree is one
        // key away even while typing.
        KeyCode::Char('b') if ctrl => return Some(Action::ToggleTree),
        // Back to typing a message, wherever the focus is: Q sits on the home
        // row right above the left Ctrl on AZERTY. G is kept as an alias.
        KeyCode::Char('q' | 'g') if ctrl => return Some(Action::FocusInput),
        KeyCode::Char('s') if ctrl => return Some(Action::ShowKeys),
        // A, top left on AZERTY, next to Q: the other most used jump.
        KeyCode::Char('a') if ctrl => return Some(Action::FocusTree),
        // Z for zoom, top left on AZERTY.
        KeyCode::Char('z') if ctrl => return Some(Action::Zoom),
        // T for team.
        KeyCode::Char('t') if ctrl => return Some(Action::ToggleSubAgent),
        KeyCode::Char('k') if ctrl => return Some(Action::ToggleDocker),
        // O for overview of what was used; P for its period.
        KeyCode::Char('o') if ctrl => return Some(Action::ToggleUsage),
        KeyCode::Char('p') if ctrl => return Some(Action::NextUsageWindow),
        // Only the window tells Ctrl-M from Enter: a terminal sends both
        // the same, which is fine, as only the window can be see-through.
        KeyCode::Char('m') if ctrl => return Some(Action::ToggleSeeThrough),
        KeyCode::Char('n') if ctrl => return Some(Action::NextMap),
        // F for find, as in most applications.
        KeyCode::Char('f') if ctrl => return Some(Action::Search),
        // E, right above the left Ctrl key on AZERTY and QWERTY keyboards alike.
        KeyCode::Char('e') if ctrl => return Some(Action::PickModel),
        _ => {}
    }

    if let Some(pending) = pending {
        return match (pending, key.code) {
            (Pending::Leader, KeyCode::Char('n')) => Some(Action::ToggleTree),
            (Pending::Leader, KeyCode::Char('c')) => Some(Action::ShowChat),
            (Pending::Leader, KeyCode::Char('i')) => Some(Action::FocusInput),
            (Pending::Leader, KeyCode::Char('d')) => Some(Action::ToggleDocker),
            (Pending::Leader, KeyCode::Char('m')) => Some(Action::PickModel),
            (Pending::Window, KeyCode::Char('w')) => Some(Action::FocusNext),
            (Pending::Window, KeyCode::Char('h') | KeyCode::Left) => Some(Action::FocusLeft),
            (Pending::Window, KeyCode::Char('l') | KeyCode::Right) => Some(Action::FocusRight),
            _ => None,
        };
    }

    match mode {
        Mode::Normal => normal(focus, key, ctrl),
        Mode::Insert | Mode::Command | Mode::Search => match key.code {
            KeyCode::Esc => Some(Action::Enter(Mode::Normal)),
            KeyCode::Enter => Some(Action::Submit),
            KeyCode::Backspace => Some(Action::Backspace),
            KeyCode::Char('w') if ctrl => Some(Action::DeleteWord),
            KeyCode::Char('u') if ctrl => Some(Action::ClearLine),
            KeyCode::Left => Some(Action::Left),
            KeyCode::Right => Some(Action::Right),
            KeyCode::Home => Some(Action::Home),
            KeyCode::End => Some(Action::End),
            KeyCode::Up if mode == Mode::Insert => Some(Action::Recall(-1)),
            KeyCode::Down if mode == Mode::Insert => Some(Action::Recall(1)),
            KeyCode::Up => Some(Action::Move(-1)),
            KeyCode::Down => Some(Action::Move(1)),
            KeyCode::PageUp => Some(Action::HalfPage(false)),
            KeyCode::PageDown => Some(Action::HalfPage(true)),
            KeyCode::Tab if mode == Mode::Insert => Some(Action::FocusNext),
            KeyCode::Char(c) if !ctrl => Some(Action::Insert(c)),
            _ => None,
        },
    }
}

fn normal(focus: Focus, key: KeyEvent, ctrl: bool) -> Option<Action> {
    match key.code {
        KeyCode::Char('w') if ctrl => return Some(Action::Wait(Pending::Window)),
        KeyCode::Char(',') => return Some(Action::Wait(Pending::Leader)),
        KeyCode::Tab => return Some(Action::FocusNext),
        KeyCode::Char('i' | 'a') => return Some(Action::Enter(Mode::Insert)),
        KeyCode::Char(':') => return Some(Action::Enter(Mode::Command)),
        KeyCode::Char('?') => return Some(Action::ShowKeys),
        KeyCode::Up => return Some(Action::Move(-1)),
        KeyCode::Down => return Some(Action::Move(1)),
        KeyCode::PageUp => return Some(Action::HalfPage(false)),
        KeyCode::PageDown => return Some(Action::HalfPage(true)),
        KeyCode::Home => return Some(Action::Top),
        KeyCode::End => return Some(Action::Bottom),
        _ => {}
    }
    match focus {
        Focus::Tree => match key.code {
            KeyCode::Enter | KeyCode::Right => Some(Action::Open),
            KeyCode::Left => Some(Action::Collapse),
            KeyCode::Char('q') | KeyCode::Esc => Some(Action::ClosePane),
            _ => None,
        },
        // Keys in the open file go to its editor, which follows Vim: see
        // `editor.rs`. Only Tab, Ctrl-W and the leader reach this function.
        Focus::File => None,
        Focus::Docker | Focus::SubAgent => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Some(Action::ClosePane),
            _ => None,
        },
        // Up and Down select replies here, Enter and Space fold them; Page
        // keys and the wheel scroll.
        Focus::Chat => match key.code {
            KeyCode::Enter | KeyCode::Char(' ') => Some(Action::Fold),
            KeyCode::Char('y') => Some(Action::CopySelected),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn letters_type_in_insert_mode() {
        assert_eq!(
            action(Mode::Insert, Focus::Chat, None, key(KeyCode::Char(','))),
            Some(Action::Insert(','))
        );
    }

    #[test]
    fn comma_n_toggles_the_tree_from_normal_mode() {
        assert_eq!(
            action(Mode::Normal, Focus::Chat, None, key(KeyCode::Char(','))),
            Some(Action::Wait(Pending::Leader))
        );
        assert_eq!(
            action(
                Mode::Normal,
                Focus::Chat,
                Some(Pending::Leader),
                key(KeyCode::Char('n'))
            ),
            Some(Action::ToggleTree)
        );
    }

    #[test]
    fn arrows_drive_the_tree() {
        let tree = |code| action(Mode::Normal, Focus::Tree, None, key(code));
        assert_eq!(tree(KeyCode::Down), Some(Action::Move(1)));
        assert_eq!(tree(KeyCode::Right), Some(Action::Open));
        assert_eq!(tree(KeyCode::Left), Some(Action::Collapse));
        assert_eq!(tree(KeyCode::Char('j')), None);
    }

    #[test]
    fn ctrl_c_cancels_and_ctrl_b_toggles_the_tree_in_every_mode() {
        for mode in [Mode::Normal, Mode::Insert, Mode::Command, Mode::Search] {
            assert_eq!(
                action(mode, Focus::Chat, None, ctrl('c')),
                Some(Action::Cancel)
            );
            assert_eq!(
                action(mode, Focus::Chat, None, ctrl('b')),
                Some(Action::ToggleTree)
            );
        }
    }

    #[test]
    fn ctrl_g_and_ctrl_k_work_in_every_mode() {
        for mode in [Mode::Normal, Mode::Insert, Mode::Command] {
            for focus in [Focus::Tree, Focus::Chat, Focus::Docker] {
                assert_eq!(
                    action(mode, focus, None, ctrl('g')),
                    Some(Action::FocusInput)
                );
                assert_eq!(
                    action(mode, focus, None, ctrl('k')),
                    Some(Action::ToggleDocker)
                );
            }
        }
    }

    #[test]
    fn ctrl_w_then_an_arrow_moves_between_panes() {
        assert_eq!(
            action(Mode::Normal, Focus::Chat, None, ctrl('w')),
            Some(Action::Wait(Pending::Window))
        );
        assert_eq!(
            action(
                Mode::Normal,
                Focus::Chat,
                Some(Pending::Window),
                key(KeyCode::Left)
            ),
            Some(Action::FocusLeft)
        );
    }
}
