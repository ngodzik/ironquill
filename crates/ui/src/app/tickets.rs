//! The branch's tickets in the interface: `/ticket` links one, notes what
//! was verified elsewhere, checks progress; the pane (Ctrl-L) shows them,
//! and a click on an item or a file shows its pieces of the diff.

use super::{App, Effect};
use crate::tickets::{Progress, Ticket, Tickets};
use universe_core::ModelId;

/// What the view on the left shows of the tickets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shown {
    /// The pieces that serve an item: ticket, item.
    Item(usize, usize),
    /// The pieces of a file.
    File(String),
}

/// The pieces shown, and how many lines around each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketView {
    /// What is shown.
    pub shown: Shown,
    /// Lines of context above and below each piece: three, then ten more
    /// for each click on an edge.
    pub context: usize,
}

impl App {
    /// Takes the tickets kept for `branch`, as the host read them.
    pub fn tickets_loaded(&mut self, branch: Option<String>, tickets: Tickets) {
        self.tickets_branch = branch;
        self.tickets_kept = Some(tickets.clone());
        self.tickets = tickets;
    }

    /// The branch's tickets.
    pub fn tickets(&self) -> &Tickets {
        &self.tickets
    }

    /// What is being done with them, while it is: `Reading ENG-1…`.
    pub fn tickets_busy(&self) -> Option<&str> {
        self.tickets_busy.as_deref()
    }

    /// The tickets, when they changed since last kept, with their branch.
    pub fn tickets_to_keep(&mut self) -> Option<(String, Tickets)> {
        let branch = self.tickets_branch.clone()?;
        if self.tickets_kept.as_ref() == Some(&self.tickets) {
            return None;
        }
        self.tickets_kept = Some(self.tickets.clone());
        Some((branch, self.tickets.clone()))
    }

    /// The model for fresh work, and the models to pick the cheapest from
    /// when there is none: an agent cannot answer one question.
    fn fresh_or_cheapest(&self) -> (Option<ModelId>, Vec<ModelId>) {
        let fresh = self.fresh_model().filter(|m| m.delegate().is_none());
        (fresh, self.models())
    }

    /// `/ticket ...`: alone, the pane; `note <text>`, a note for the judge;
    /// `check`, progress; an id or a link, the ticket linked.
    pub(super) fn ticket_command(&mut self, argument: Option<String>) -> Option<Effect> {
        let Some(argument) = argument.map(|a| a.trim().to_owned()) else {
            self.tickets_open = true;
            self.usage_open = false;
            self.branch = None;
            return None;
        };
        if let Some(note) = argument.strip_prefix("note ") {
            self.tickets.notes.push(note.trim().to_owned());
            self.info("Noted for the next check of progress");
            return None;
        }
        if argument == "check" {
            return self.check_progress();
        }
        self.read_ticket(&argument)
    }

    /// Reads the ticket `argument` names, an id or a link.
    fn read_ticket(&mut self, argument: &str) -> Option<Effect> {
        if self.tickets_branch.is_none() {
            self.error("Tickets are linked to a branch: this project is on none");
            return None;
        }
        let named = match crate::tickets::recognize(&self.settings.trackers, argument) {
            Ok(named) => named,
            Err(why) => {
                self.error(why);
                return None;
            }
        };
        let Some(external) = self.settings.external.clone() else {
            self.error("No MCP server is declared: tickets are read through one, [mcp.<name>]");
            return None;
        };
        let tracker = self.settings.trackers.get(&named.tracker)?.clone();
        let (model, candidates) = self.fresh_or_cheapest();
        self.tickets_busy = Some(format!("Reading {}…", named.id));
        self.tickets_open = true;
        Some(Effect::ReadTicket {
            named,
            tracker,
            external,
            model,
            candidates,
        })
    }

    /// Asks for the branch's work to be judged against every ticket.
    pub fn check_progress(&mut self) -> Option<Effect> {
        if self.tickets.tickets.is_empty() {
            self.error("No ticket linked to this branch: /ticket <id or link>");
            return None;
        }
        let (model, candidates) = self.fresh_or_cheapest();
        self.tickets_busy = Some("Checking progress…".to_owned());
        Some(Effect::CheckProgress {
            tickets: self.tickets.tickets.clone(),
            notes: self.tickets.notes.clone(),
            model,
            candidates,
        })
    }

    /// A ticket read, or why it could not be.
    pub(super) fn ticket_read(&mut self, read: Result<Ticket, String>) {
        self.tickets_busy = None;
        match read {
            Ok(ticket) => {
                let said = format!(
                    "{} linked: {} ({} items)",
                    ticket.id,
                    ticket.title,
                    ticket.items.len()
                );
                match self
                    .tickets
                    .tickets
                    .iter_mut()
                    .find(|t| t.tracker == ticket.tracker && t.id == ticket.id)
                {
                    Some(kept) => *kept = ticket,
                    None => self.tickets.tickets.push(ticket),
                }
                // The items changed: the last check no longer says them.
                self.tickets.progress = None;
                self.info(said);
            }
            Err(why) => self.error(why),
        }
    }

    /// A check of progress, or why it failed.
    pub(super) fn progress_checked(&mut self, checked: Result<Progress, String>) {
        self.tickets_busy = None;
        match checked {
            Ok(progress) => {
                let done = progress
                    .items
                    .iter()
                    .flatten()
                    .filter(|p| p.as_ref().is_some_and(|p| p.done))
                    .count();
                let all: usize = self.tickets.tickets.iter().map(|t| t.items.len()).sum();
                self.info(format!("Progress: {done} of {all} items done (Ctrl-L)"));
                self.tickets.progress = Some(progress);
                self.tickets_open = true;
            }
            Err(why) => self.error(why),
        }
    }

    /// Unlinks the ticket at `at`.
    pub fn unlink_ticket(&mut self, at: usize) {
        if at < self.tickets.tickets.len() {
            let gone = self.tickets.tickets.remove(at);
            self.tickets.progress = None;
            self.ticket_view = None;
            self.info(format!("{} unlinked from this branch", gone.id));
        }
    }

    /// Reads the ticket at `at` again.
    pub fn reread_ticket(&mut self, at: usize) -> Option<Effect> {
        let ticket = self.tickets.tickets.get(at)?.clone();
        let link = ticket.id.clone();
        self.read_ticket(&link)
    }

    /// Takes the note at `at` away.
    pub fn remove_note(&mut self, at: usize) {
        if at < self.tickets.notes.len() {
            self.tickets.notes.remove(at);
        }
    }

    /// Ticks or unticks `path` as looked at, kept across checks.
    pub fn toggle_looked_at(&mut self, path: &str) {
        if !self.tickets.looked_at.remove(path) {
            self.tickets.looked_at.insert(path.to_owned());
        }
    }

    /// The items of the ticket at `at`, as a list to copy.
    pub fn ticket_items_text(&self, at: usize) -> Option<String> {
        let ticket = self.tickets.tickets.get(at)?;
        Some(
            ticket
                .items
                .iter()
                .map(|i| format!("- {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    /// Puts the items of the ticket at `at` in the message box.
    pub fn ticket_items_to_input(&mut self, at: usize) {
        if let Some(text) = self.ticket_items_text(at) {
            let id = self.tickets.tickets[at].id.clone();
            self.on_paste(&format!("{id}:\n{text}\n"));
        }
    }

    /// Shows the pieces of an item or of a file on the left; the same
    /// again closes them.
    pub fn show_ticket_part(&mut self, shown: Shown) {
        if self.ticket_view.as_ref().is_some_and(|v| v.shown == shown) {
            self.ticket_view = None;
        } else {
            self.ticket_view = Some(TicketView { shown, context: 3 });
        }
    }

    /// What the view on the left shows, if anything.
    pub fn ticket_view(&self) -> Option<&TicketView> {
        self.ticket_view.as_ref()
    }

    /// Ten more lines around each piece, as a click on an edge asks.
    pub fn widen_ticket_view(&mut self) {
        if let Some(view) = &mut self.ticket_view {
            view.context += 10;
        }
    }
}
