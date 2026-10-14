//! What the branch changed since it left the main one (Ctrl-R), on the
//! right: the API's routes added, changed or removed, each unfolding to all
//! the API view would show of it, its additions green, its removals struck
//! through, its changes `old → new`; and the database's migrations and
//! models, a click away from the database view showing only their tables.

use std::collections::{BTreeSet, HashSet};

use bevy_egui::egui::{self, Color32, CornerRadius, Frame, Margin, RichText, Sense, Stroke, Ui};
use universe_codemap::{CodeMap, Delta, OperationLine, RouteChange, compare_operations};
use universe_ui::{App, Effect, MapView};

use crate::database_view::DatabaseView;
use crate::review_view::{ReviewView, badge, file_link, schema_line};
use crate::theme::{CODE, DIM, EDGE, GREEN, RED, TEXT, YELLOW};

/// The pane's own memory: the branch read, and the routes unfolded.
#[derive(Default)]
pub(crate) struct BranchPane {
    pub(crate) review: ReviewView,
    unfolded: HashSet<(String, String)>,
}

/// Draws the pane. `map` is the project's, for what serves a route and what
/// calls it.
pub(crate) fn show(
    ui: &mut Ui,
    pane: &mut BranchPane,
    map: Option<&CodeMap>,
    database: &mut DatabaseView,
    app: &mut App,
    effects: &mut Vec<Effect>,
) {
    // Labels are selectable by default and swallow clicks: a row would
    // react only on its arrow.
    ui.style_mut().interaction.selectable_labels = false;
    let Some(changes) = app.branch_pane() else {
        return;
    };
    ui.label(RichText::new("This branch").size(20.0).color(TEXT));
    ui.label(
        RichText::new(format!(
            "since {} · {} files",
            changes.base_line,
            changes.files.len()
        ))
        .size(12.0)
        .color(DIM),
    );
    let Some(review) = pane.review.review().cloned() else {
        ui.add_space(12.0);
        ui.label(RichText::new("Reading the branch…").color(DIM));
        ui.ctx().request_repaint();
        return;
    };
    let mut open: Option<String> = None;

    heading(ui, "API", review.routes.len());
    if review.routes.is_empty() {
        ui.label(RichText::new("no route changed").size(12.5).color(DIM));
    }
    for route in &review.routes {
        let key = (route.method.clone(), route.path.clone());
        let unfolded = pane.unfolded.contains(&key);
        let row = Frame::new()
            .inner_margin(Margin::symmetric(6, 4))
            .corner_radius(CornerRadius::same(6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(if unfolded { "▾" } else { "▸" }).color(DIM));
                    badge(ui, route.delta);
                    crate::api_view::method_pill(ui, &route.method, 12.0);
                    ui.label(
                        RichText::new(&route.path)
                            .monospace()
                            .size(13.0)
                            .color(TEXT),
                    );
                });
            })
            .response;
        // A click anywhere on the line unfolds it.
        let row = ui
            .interact(row.rect, ui.id().with(("route", &key)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if row.clicked() {
            if unfolded {
                pane.unfolded.remove(&key);
            } else {
                pane.unfolded.insert(key.clone());
            }
        }
        if unfolded {
            route_sheet(ui, route, map, effects, &mut open);
        }
    }

    let tables = changed_tables(&review);
    heading(
        ui,
        "Database",
        review.migrations.len() + review.models.len(),
    );
    if review.migrations.is_empty() && review.models.is_empty() {
        ui.label(
            RichText::new("no migration or model changed")
                .size(12.5)
                .color(DIM),
        );
    }
    for (path, migration) in &review.migrations {
        ui.add_space(4.0);
        ui.label(RichText::new(&migration.title).size(13.5).color(TEXT));
        if file_link(ui, path, 11.5) {
            open = Some(path.clone());
        }
        for step in &migration.steps {
            schema_line(ui, step);
        }
    }
    for (path, steps) in &review.models {
        ui.add_space(4.0);
        if file_link(ui, path, 12.5) {
            open = Some(path.clone());
        }
        for step in steps {
            schema_line(ui, step);
        }
    }
    if !tables.is_empty() {
        ui.add_space(6.0);
        let link = ui
            .add(
                egui::Label::new(
                    RichText::new(format!(
                        "Show these {} tables, and those their keys link, in the database view",
                        tables.len()
                    ))
                    .size(12.5)
                    .color(crate::theme::LINK),
                )
                .sense(Sense::click()),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if link.clicked() {
            database.focus(tables.into_iter().collect());
            app.show_map(Some(MapView::Database));
        }
    }
    if let Some(path) = open {
        app.open_path(app.root().join(path));
    }
}

/// The tables the migrations and models change.
fn changed_tables(review: &universe_codemap::Review) -> BTreeSet<String> {
    review
        .migrations
        .iter()
        .flat_map(|(_, m)| &m.steps)
        .chain(review.models.iter().flat_map(|(_, steps)| steps))
        .filter_map(|step| step.table.clone())
        .collect()
}

fn heading(ui: &mut Ui, title: &str, count: usize) {
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(16.0).color(TEXT));
        ui.label(RichText::new(count.to_string()).size(12.0).color(DIM));
    });
    ui.add_space(2.0);
}

/// All of a route, compared before and after; then how to call it, what
/// serves it and what calls it, as the project is now.
fn route_sheet(
    ui: &mut Ui,
    route: &RouteChange,
    map: Option<&CodeMap>,
    effects: &mut Vec<Effect>,
    open: &mut Option<String>,
) {
    Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(Stroke::new(1.0, EDGE))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            for line in compare_operations(route.before.as_ref(), route.after.as_ref()) {
                operation_line(ui, &line);
            }
            if let Some(after) = &route.after {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("curl").size(11.5).color(DIM));
                    if ui.small_button(RichText::new("copy").color(DIM)).clicked() {
                        effects.push(Effect::Copy(after.curl()));
                    }
                });
                ui.label(
                    RichText::new(after.curl())
                        .monospace()
                        .size(11.5)
                        .color(CODE),
                );
            }
            // What serves it and calls it, from the project as it is now.
            let now = map.and_then(|m| {
                m.operations
                    .iter()
                    .find(|o| o.method == route.method && o.path == route.path)
                    .map(|o| (m, o))
            });
            if let Some((map, operation)) = now {
                if let Some((file, line)) = operation.handler {
                    ui.add_space(4.0);
                    ui.label(RichText::new("served by").size(11.5).color(DIM));
                    let path = &map.nodes[file].path;
                    if file_link(ui, &format!("{path}:{line}"), 12.0) {
                        *open = Some(path.clone());
                    }
                }
                if !operation.callers.is_empty() {
                    ui.label(RichText::new("called from").size(11.5).color(DIM));
                    for caller in &operation.callers {
                        let path = &map.nodes[*caller].path;
                        if file_link(ui, path, 12.0) {
                            *open = Some(path.clone());
                        }
                    }
                }
            }
            if route.delta == Delta::Removed {
                ui.label(
                    RichText::new("removed by this branch")
                        .size(11.5)
                        .color(RED),
                );
            }
        });
}

/// One line of a route compared: what stayed in the text's colour, what
/// was added in green, what was removed struck through in red, and what
/// changed as `old → new`.
fn operation_line(ui: &mut Ui, line: &OperationLine) {
    let key = |ui: &mut Ui, key: &str| {
        ui.label(RichText::new(key).monospace().size(11.5).color(DIM));
    };
    let text = |value: &str, colour: Color32| RichText::new(value).size(12.5).color(colour);
    ui.horizontal_wrapped(|ui| match line {
        OperationLine::Same { key: k, value } => {
            key(ui, k);
            ui.label(text(value, TEXT));
        }
        OperationLine::Added { key: k, value } => {
            key(ui, k);
            ui.label(text(value, GREEN));
        }
        OperationLine::Removed { key: k, value } => {
            key(ui, k);
            ui.label(text(value, RED).strikethrough());
        }
        OperationLine::Changed {
            key: k,
            before,
            after,
        } => {
            key(ui, k);
            ui.label(text(before, RED).strikethrough());
            ui.label(text("→", YELLOW));
            ui.label(text(after, GREEN));
        }
    });
}
