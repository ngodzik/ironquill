//! What a branch changed, read as a reviewer reads it (Ctrl-N, while a
//! review or a piece of work is looked at): the API's routes added, removed
//! or changed; what the migrations do and what the models' tables gained
//! or lost; every changed file in its area, each a click from open.
//!
//! The texts before and after are read with git on a thread of its own,
//! once per change set, and read by `ironquill-codemap`: the window never
//! waits on them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use bevy_egui::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Margin, RichText, ScrollArea, Sense,
    Ui, UiBuilder, pos2, vec2,
};
use ironquill_codemap::{Delta, Review, RouteChange, SchemaChange};
use ironquill_tools::Change;
use ironquill_ui::changes::ChangeSet;
use ironquill_ui::{App, MapView};

use crate::api_view::{self, ApiView};
use crate::plan::Plan;
use crate::theme::{self, DIM, EDGE, GREEN, RAISED, RED, TEXT, YELLOW};

/// A branch's changes, read: kept until they change.
#[derive(Default)]
pub(crate) struct ReviewView {
    /// What the review was read for: the base and the files.
    key: Option<String>,
    reading: Option<Receiver<Read>>,
    review: Option<Review>,
    /// For each changed file, its blocks at the top (a function with its
    /// decorators, a class) as ranges of lines from 1, and whether the
    /// branch changed each.
    blocks: HashMap<String, Vec<(usize, usize, bool)>>,
}

/// What the thread reads: the review, and each file's blocks.
struct Read {
    review: Review,
    blocks: HashMap<String, Vec<(usize, usize, bool)>>,
}

impl ReviewView {
    /// Reads the changes looked at, on a thread, when they differ from
    /// those last read; forgets them when none are.
    pub(crate) fn update(&mut self, root: &Path, changes: Option<&ChangeSet>) {
        let Some(changes) = changes else {
            *self = Self::default();
            return;
        };
        let key = format!(
            "{} {}",
            changes.base,
            changes
                .files
                .iter()
                .map(|f| format!("{}+{}-{}", f.path, f.added, f.removed))
                .collect::<Vec<_>>()
                .join(" ")
        );
        if self.key.as_deref() != Some(key.as_str()) {
            self.key = Some(key);
            let (send, receive) = mpsc::channel();
            let root = root.to_owned();
            let base = changes.base.clone();
            let files: Vec<(String, Change)> = changes
                .files
                .iter()
                .map(|f| (f.path.clone(), f.change.clone()))
                .collect();
            std::thread::spawn(move || {
                let _ = send.send(read(&root, &base, &files));
            });
            self.reading = Some(receive);
        }
        if let Some(reading) = &self.reading {
            match reading.try_recv() {
                Ok(read) => {
                    self.review = Some(read.review);
                    self.blocks = read.blocks;
                    self.reading = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.reading = None,
            }
        }
    }

    /// The changes, once read.
    pub(crate) fn review(&self) -> Option<&Review> {
        self.review.as_ref()
    }

    /// Whether the branch changed the block at the top of `path` that
    /// holds `line`, from 1: the function that serves a route. Without
    /// blocks, as for a file not read, whether it changed the file at all.
    pub(crate) fn changed_at(&self, path: &str, line: usize) -> bool {
        match self.blocks.get(path) {
            Some(blocks) => blocks
                .iter()
                .any(|(start, end, changed)| *changed && *start <= line && line < *end),
            None => false,
        }
    }

    /// How a route of the API differs in the branch, if it does.
    pub(crate) fn route(&self, method: &str, path: &str) -> Option<&RouteChange> {
        self.review
            .as_ref()?
            .routes
            .iter()
            .find(|r| r.method == method && r.path == path)
    }
}

/// The blocks at the top of a file's `text`, as ranges of lines from 1: a
/// run of decorators and the function or class they adorn, or a statement,
/// each up to the next.
fn blocks(text: &str) -> Vec<(usize, usize)> {
    let lines: Vec<&str> = text.lines().collect();
    let top = |l: &str| !l.is_empty() && !l.starts_with(char::is_whitespace) && !l.starts_with('#');
    let mut starts: Vec<usize> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        // A decorator's function starts with it; a line closing what the
        // one above opened (`)`, `]`) is not a block of its own.
        let after_decorator = i > 0 && lines[i - 1].starts_with('@');
        if top(line) && !after_decorator && !line.starts_with([')', ']', '}']) {
            starts.push(i + 1);
        }
    }
    let mut found = Vec::new();
    for (k, &start) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(lines.len() + 1);
        found.push((start, end));
    }
    found
}

/// Each changed file's text before and after, read, then reviewed.
fn read(root: &Path, base: &str, files: &[(String, Change)]) -> Read {
    let revised: Vec<ironquill_codemap::Revised> = files
        .iter()
        .map(|(path, change)| {
            let was = match change {
                Change::Renamed { from } => from.as_str(),
                _ => path.as_str(),
            };
            let before = match change {
                Change::Added => None,
                _ => ironquill_tools::lines_at(root, Path::new(was), base)
                    .filter(|lines| !lines.is_empty())
                    .map(|lines| lines.join("\n")),
            };
            let after = match change {
                Change::Deleted => None,
                _ => std::fs::read_to_string(root.join(path)).ok(),
            };
            ironquill_codemap::Revised {
                path: path.clone(),
                before,
                after,
            }
        })
        .collect();
    // Which blocks of each file changed: those holding a line marked, or
    // the place lines were removed from.
    let mut found = HashMap::new();
    for file in &revised {
        let Some(after) = &file.after else {
            continue;
        };
        let now: Vec<String> = after.lines().map(str::to_owned).collect();
        let was: Vec<String> = file
            .before
            .as_deref()
            .unwrap_or("")
            .lines()
            .map(str::to_owned)
            .collect();
        let changes = ironquill_tools::line_changes(&was, &now);
        let changed = |line: usize| {
            changes
                .marks
                .get(line - 1)
                .is_some_and(|m| *m != ironquill_tools::LineMark::Same)
                || changes.removed.contains_key(&(line - 1))
        };
        let marked = blocks(after)
            .into_iter()
            .map(|(start, end)| (start, end, (start..end).any(changed)))
            .collect();
        found.insert(file.path.clone(), marked);
    }
    Read {
        review: ironquill_codemap::review(&revised),
        blocks: found,
    }
}

/// A delta's colour and its sign.
fn delta_look(delta: Delta) -> (Color32, &'static str) {
    match delta {
        Delta::Added => (GREEN, "+"),
        Delta::Removed => (RED, "−"),
        Delta::Changed => (YELLOW, "~"),
    }
}

/// A small word in a pill of its colour: NEW, CHANGED, REMOVED.
fn badge(ui: &mut Ui, delta: Delta) {
    let (colour, _) = delta_look(delta);
    let word = match delta {
        Delta::Added => "NEW",
        Delta::Removed => "REMOVED",
        Delta::Changed => "CHANGED",
    };
    Frame::new()
        .fill(colour.gamma_multiply(0.15))
        .stroke(egui::Stroke::new(1.0, colour.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(5, 0))
        .show(ui, |ui| {
            ui.label(RichText::new(word).size(10.0).strong().color(colour));
        });
}

fn section(ui: &mut Ui, title: &str, count: usize) {
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(17.0).color(TEXT));
        ui.label(RichText::new(count.to_string()).size(12.0).color(DIM));
    });
    ui.add_space(4.0);
}

/// A step of a migration or a model: its sign, its table, what it does.
fn schema_line(ui: &mut Ui, change: &SchemaChange) {
    let (colour, sign) = delta_look(change.delta);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.label(RichText::new(sign).monospace().strong().color(colour));
        if let Some(table) = &change.table {
            ui.label(RichText::new(table).monospace().color(theme::CODE));
        }
        ui.label(RichText::new(&change.what).color(TEXT));
    });
}

/// A path as a link: a click opens the file, folded to its changes.
fn file_link(ui: &mut Ui, path: &str, size: f32) -> bool {
    let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
    let mut job = egui::text::LayoutJob::default();
    if !folder.is_empty() {
        job.append(
            &format!("{folder}/"),
            0.0,
            egui::TextFormat::simple(FontId::proportional(size), DIM),
        );
    }
    job.append(
        name,
        0.0,
        egui::TextFormat::simple(FontId::proportional(size), TEXT),
    );
    job.wrap.max_rows = 1;
    job.wrap.max_width = ui.available_width();
    ui.add(egui::Label::new(job).truncate().sense(Sense::click()))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!("open {path}"))
        .clicked()
}

/// Draws the review in `ui`.
pub(crate) fn show(
    ui: &mut Ui,
    view: &mut ReviewView,
    api: &mut ApiView,
    plan: &Plan,
    app: &mut App,
) {
    let rect = ui.max_rect();
    let painter = ui.painter_at(rect);
    let corner = rect.left_top() + vec2(24.0, 22.0);
    crate::plan::switch(ui, rect, app);
    let Some(changes) = app.change_set().cloned() else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "/review or /work shows what a branch changed",
            FontId::proportional(16.0),
            DIM,
        );
        return;
    };
    painter.text(
        corner,
        Align2::LEFT_TOP,
        match changes.lens {
            ironquill_ui::changes::Lens::Review => "Review",
            ironquill_ui::changes::Lens::Work => "Work",
        },
        FontId::proportional(26.0),
        TEXT,
    );
    let (added, removed) = changes.lines();
    painter.text(
        corner + vec2(0.0, 34.0),
        Align2::LEFT_TOP,
        format!(
            "{} · {} files · +{added} −{removed} · since {}",
            app.project(),
            changes.files.len(),
            changes.base_line
        ),
        FontId::proportional(13.0),
        DIM,
    );
    let Some(review) = view.review().cloned() else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Reading the changes…",
            FontId::proportional(16.0),
            DIM,
        );
        ui.ctx().request_repaint();
        return;
    };

    let top = rect.top() + 76.0;
    let split = rect.left() + rect.width() * 0.56;
    let left = egui::Rect::from_min_max(
        pos2(rect.left() + 24.0, top),
        pos2(split - 12.0, rect.bottom() - 16.0),
    );
    let right = egui::Rect::from_min_max(
        pos2(split + 12.0, top),
        pos2(rect.right() - 24.0, rect.bottom() - 16.0),
    );
    painter.line_segment(
        [pos2(split, top + 8.0), pos2(split, rect.bottom() - 24.0)],
        egui::Stroke::new(1.0, EDGE),
    );
    let mut open: Option<String> = None;
    let mut route: Option<(String, String)> = None;

    ui.scope_builder(UiBuilder::new().max_rect(left), |ui| {
        ScrollArea::vertical()
            .id_salt("review-left")
            .auto_shrink(false)
            .show(ui, |ui| {
                section(ui, "API", review.routes.len());
                if review.routes.is_empty() {
                    ui.label(RichText::new("No route of the specs changed").color(DIM));
                }
                for change in &review.routes {
                    let shown = Frame::new()
                        .fill(RAISED)
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                badge(ui, change.delta);
                                api_view::method_pill(ui, &change.method, 11.0);
                                ui.label(RichText::new(&change.path).monospace().size(13.0).color(
                                    if change.delta == Delta::Removed {
                                        DIM
                                    } else {
                                        TEXT
                                    },
                                ));
                            });
                            for detail in &change.details {
                                ui.horizontal(|ui| {
                                    ui.add_space(8.0);
                                    ui.label(
                                        RichText::new(format!("• {detail}")).size(12.5).color(DIM),
                                    );
                                });
                            }
                        });
                    if change.delta != Delta::Removed {
                        let response = shown
                            .response
                            .interact(Sense::click())
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text("open it in the API view");
                        if response.clicked() {
                            route = Some((change.method.clone(), change.path.clone()));
                        }
                    }
                    ui.add_space(6.0);
                }

                let steps: usize = review
                    .migrations
                    .iter()
                    .map(|(_, m)| m.steps.len())
                    .sum::<usize>()
                    + review.models.iter().map(|(_, c)| c.len()).sum::<usize>();
                section(ui, "Database", steps);
                if review.migrations.is_empty() && review.models.is_empty() {
                    ui.label(RichText::new("No migration nor model changed").color(DIM));
                }
                for (path, migration) in &review.migrations {
                    Frame::new()
                        .fill(RAISED)
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("migration").size(11.0).color(theme::CYAN));
                                ui.label(RichText::new(&migration.title).size(14.0).color(TEXT));
                            });
                            if let (Some(down), Some(revision)) =
                                (&migration.down_revision, &migration.revision)
                            {
                                ui.label(
                                    RichText::new(format!("revision {down} → {revision}"))
                                        .monospace()
                                        .size(11.0)
                                        .color(DIM),
                                );
                            }
                            for step in &migration.steps {
                                schema_line(ui, step);
                            }
                            if migration.steps.is_empty() {
                                ui.label(
                                    RichText::new("its upgrade names no step this reads")
                                        .size(12.0)
                                        .color(DIM),
                                );
                            }
                            if file_link(ui, path, 11.5) {
                                open = Some(path.clone());
                            }
                        });
                    ui.add_space(6.0);
                }
                for (path, changes) in &review.models {
                    Frame::new()
                        .fill(RAISED)
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new("model").size(11.0).color(theme::MAGENTA));
                            for change in changes {
                                schema_line(ui, change);
                            }
                            if file_link(ui, path, 11.5) {
                                open = Some(path.clone());
                            }
                        });
                    ui.add_space(6.0);
                }
                ui.add_space(16.0);
            });
    });

    ui.scope_builder(UiBuilder::new().max_rect(right), |ui| {
        ScrollArea::vertical()
            .id_salt("review-right")
            .auto_shrink(false)
            .show(ui, |ui| {
                section(ui, "Files", changes.files.len());
                for (area, files) in &review.areas {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(area.name()).size(13.5).strong().color(TEXT));
                        ui.label(RichText::new(files.len().to_string()).size(11.5).color(DIM));
                    });
                    for path in files {
                        let file = changes.find(path);
                        ui.horizontal(|ui| {
                            let (letter, colour) = match file.map(|f| &f.change) {
                                Some(Change::Added) => ("A", GREEN),
                                Some(Change::Deleted) => ("D", RED),
                                Some(Change::Renamed { .. }) => ("R", theme::CYAN),
                                _ => ("M", YELLOW),
                            };
                            ui.label(RichText::new(letter).monospace().size(11.0).color(colour));
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if let Some(file) = file {
                                    ui.label(
                                        RichText::new(format!("−{}", file.removed))
                                            .size(11.0)
                                            .color(RED),
                                    );
                                    ui.label(
                                        RichText::new(format!("+{}", file.added))
                                            .size(11.0)
                                            .color(GREEN),
                                    );
                                }
                                ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                                    if file_link(ui, path, 12.5) {
                                        open = Some(path.clone());
                                    }
                                });
                            });
                        });
                    }
                }
                ui.add_space(16.0);
            });
    });

    if let Some(path) = open {
        let path: PathBuf = app.root().join(path);
        app.show_map(None);
        app.open_path(path);
    }
    if let Some((method, path)) = route
        && let Some(map) = plan.map()
    {
        api.choose(&map.operations, &method, &path);
        app.show_map(Some(MapView::Api));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_function_holds_its_decorators_and_ends_where_the_next_starts() {
        let text = "import x\n\n@router.get(\"/a\")\n@limit\ndef a():\n    return 1\n\n\nB = (\n    1,\n)\ndef c():\n    pass\n";
        assert_eq!(blocks(text), [(1, 3), (3, 9), (9, 12), (12, 14)]);
    }
}
