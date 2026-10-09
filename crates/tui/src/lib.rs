//! The ironquill terminal interface.
//!
//! The state, the keys and what each does live in `ironquill-ui`, which
//! knows nothing of terminals. This crate only:
//! - draws that state with Ratatui (`view`), changing nothing;
//! - shows pictures with Kitty's protocol (`graphics`, `pictures`);
//! - runs the loop below, which turns the terminal's events into the
//!   state's own, carries out the `Effect`s the state returns and wires the
//!   agent in.

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod graphics;
mod markdown;
mod pictures;
mod view;
mod wrap;

use std::cell::RefCell;
use std::sync::Arc;

use futures::StreamExt;
use ironquill_core::{ChatModel, Delegate};
use ironquill_tools::Workspace;
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as TermEvent, EventStream, KeyEventKind,
};
use ratatui::crossterm::execute;
use tokio::sync::mpsc;

use ironquill_ui::{App, Defaults, Host, Settings, Start, Waiting, input};

pub use error::TuiError;

/// Opens the interface in the current terminal and runs until the person quits.
///
/// `workspace` is the project the agent works on.
///
/// # Errors
///
/// [`TuiError::Terminal`] when the terminal cannot be set up or drawn to.
pub async fn run<M, D>(
    model: Arc<M>,
    delegate: Arc<D>,
    workspace: Workspace,
    settings: Settings,
    start: Start,
) -> Result<(), TuiError>
where
    M: ChatModel + 'static,
    D: Delegate + 'static,
{
    let mut terminal = ratatui::try_init().map_err(TuiError::Terminal)?;
    // Clicks and the wheel reach the interface. Selecting text with the mouse
    // then needs Shift held, as in most terminal applications that do this.
    // Pasted text arrives in one piece, so that its line breaks do not send
    // the message.
    let mouse = execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)
        .map_err(TuiError::Terminal);
    let result = match mouse {
        Ok(()) => event_loop(&mut terminal, model, delegate, workspace, settings, start).await,
        Err(e) => Err(e),
    };
    // Restored whatever happened, or the person's shell is left in raw mode.
    let _ = execute!(
        std::io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste
    );
    ratatui::restore();
    result
}

async fn event_loop<M, D>(
    terminal: &mut ratatui::DefaultTerminal,
    model: Arc<M>,
    delegate: Arc<D>,
    workspace: Workspace,
    settings: Settings,
    start: Start,
) -> Result<(), TuiError>
where
    M: ChatModel + 'static,
    D: Delegate + 'static,
{
    // Whether the terminal draws images is found once, from what it says
    // of itself.
    let terminal_kind = graphics::detect(settings.images, |name| std::env::var(name).ok());
    let renderer =
        pictures::Renderer::find(settings.mermaid.as_deref(), pictures::Renderer::installed);
    let cache = Defaults::path().map(|p| p.with_file_name("cache").join("diagrams"));
    let mut screen = terminal_kind.map(graphics::Screen::new);
    let gallery = RefCell::new(pictures::Gallery::new(workspace.root().to_owned()));
    gallery
        .borrow_mut()
        .show_in(terminal_kind, renderer.clone(), cache.clone());
    set_cell_size(&gallery);
    let mut app = App::new(settings, workspace.root().to_owned());
    app.show_pictures(gallery.borrow().shows());
    let (diagram_tx, mut diagram_rx) =
        mpsc::unbounded_channel::<(u64, Result<std::path::PathBuf, String>)>();
    let mut host = Host::new(model, delegate, workspace);
    host.start(&mut app, start).await;
    let mut keys = EventStream::new();
    // The terminal's title follows the conversation's name.
    let mut title = String::new();

    while !app.should_quit() {
        terminal
            .draw(|frame| view::render(frame, &app, &gallery))
            .map_err(TuiError::Terminal)?;
        // The images just drawn reach the terminal; diagrams first seen are
        // drawn meanwhile, each on a thread of its own.
        let wanted = gallery.borrow_mut().take_wanted();
        if let Some(screen) = screen.as_mut()
            && !wanted.is_empty()
        {
            let (escapes, failed) = screen.show(&wanted);
            gallery.borrow_mut().failed(&failed);
            write_raw(&escapes);
        }
        let to_draw = gallery.borrow_mut().take_to_draw();
        for (key, source) in to_draw {
            let (renderer, cache, tx) = (renderer.clone(), cache.clone(), diagram_tx.clone());
            std::thread::spawn(move || {
                let result = match (renderer, cache) {
                    (Some(renderer), Some(cache)) => renderer.draw(&source, &cache),
                    _ => Err("nothing draws diagrams".into()),
                };
                let _ = tx.send((key, result));
            });
        }
        let name = app.name();
        if name != title {
            let _ = execute!(
                std::io::stdout(),
                ratatui::crossterm::terminal::SetTitle(format!("{name} · ironquill"))
            );
            title = name;
        }

        let queued = app.take_queued();
        let waiting = Waiting::of(&app);
        let effect = if queued.is_some() {
            queued
        } else {
            tokio::select! {
                key = keys.next() => match key {
                    Some(Ok(TermEvent::Key(key))) if key.kind == KeyEventKind::Press => app.on_key(from_terminal_key(key)),
                    Some(Ok(TermEvent::Paste(text))) => {
                        app.on_paste(&text);
                        None
                    }
                    Some(Ok(TermEvent::Mouse(mouse))) => app.on_mouse(from_terminal_mouse(mouse)),
                    Some(Ok(TermEvent::Resize(..))) => {
                        set_cell_size(&gallery);
                        None
                    }
                    Some(Ok(_)) => None,
                    Some(Err(e)) => return Err(TuiError::Terminal(e)),
                    None => break,
                },
                Some((key, result)) = diagram_rx.recv() => {
                    gallery.borrow_mut().drawn(key, &result);
                    if let Err(reason) = result {
                        app.report_error(format!("The diagram could not be drawn: {reason}"));
                    }
                    None
                },
                incoming = host.next(waiting) => host.receive(&mut app, incoming).await,
            }
        };
        host.carry_out(&mut app, effect).await;
    }

    // The terminal keeps images until told to forget them.
    if let Some(screen) = screen.as_mut() {
        write_raw(&screen.clear());
    }
    Ok(())
}

/// Writes escapes straight to the terminal, past the drawing: images are
/// sent this way. A failure only costs the image.
fn write_raw(escapes: &str) {
    use std::io::Write;
    if escapes.is_empty() {
        return;
    }
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(escapes.as_bytes());
    let _ = out.flush();
}

/// Tells the pictures how big a cell is, in pixels, as the terminal says.
fn set_cell_size(gallery: &RefCell<pictures::Gallery>) {
    if let Ok(size) = ratatui::crossterm::terminal::window_size()
        && size.columns > 0
        && size.rows > 0
    {
        gallery
            .borrow_mut()
            .set_cell((size.width / size.columns, size.height / size.rows));
    }
}

/// A key as the terminal reports it, as the state reads it.
fn from_terminal_key(key: ratatui::crossterm::event::KeyEvent) -> input::KeyEvent {
    use input::KeyCode;
    use ratatui::crossterm::event::KeyCode as Term;
    let code = match key.code {
        Term::Char(c) => KeyCode::Char(c),
        Term::Enter => KeyCode::Enter,
        Term::Esc => KeyCode::Esc,
        Term::Tab => KeyCode::Tab,
        Term::Backspace => KeyCode::Backspace,
        Term::Delete => KeyCode::Delete,
        Term::Up => KeyCode::Up,
        Term::Down => KeyCode::Down,
        Term::Left => KeyCode::Left,
        Term::Right => KeyCode::Right,
        Term::Home => KeyCode::Home,
        Term::End => KeyCode::End,
        Term::PageUp => KeyCode::PageUp,
        Term::PageDown => KeyCode::PageDown,
        _ => KeyCode::Other,
    };
    input::KeyEvent::new(code, from_terminal_modifiers(key.modifiers))
}

/// A click or a turn of the wheel as the terminal reports it, as the state
/// reads it.
fn from_terminal_mouse(mouse: ratatui::crossterm::event::MouseEvent) -> input::MouseEvent {
    use input::{MouseButton, MouseEventKind};
    use ratatui::crossterm::event::{MouseButton as Button, MouseEventKind as Kind};
    let kind = match mouse.kind {
        Kind::Down(Button::Left) => MouseEventKind::Down(MouseButton::Left),
        Kind::Down(Button::Right) => MouseEventKind::Down(MouseButton::Right),
        Kind::Down(Button::Middle) => MouseEventKind::Down(MouseButton::Middle),
        Kind::ScrollUp => MouseEventKind::ScrollUp,
        Kind::ScrollDown => MouseEventKind::ScrollDown,
        _ => MouseEventKind::Other,
    };
    input::MouseEvent {
        kind,
        column: mouse.column,
        row: mouse.row,
        modifiers: from_terminal_modifiers(mouse.modifiers),
    }
}

fn from_terminal_modifiers(held: ratatui::crossterm::event::KeyModifiers) -> input::KeyModifiers {
    use input::KeyModifiers;
    use ratatui::crossterm::event::KeyModifiers as Term;
    [
        (Term::SHIFT, KeyModifiers::SHIFT),
        (Term::CONTROL, KeyModifiers::CONTROL),
        (Term::ALT, KeyModifiers::ALT),
    ]
    .into_iter()
    .filter(|(term, _)| held.contains(*term))
    .fold(KeyModifiers::NONE, |all, (_, ours)| all | ours)
}
