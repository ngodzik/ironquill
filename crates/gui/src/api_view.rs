//! The codebase's API (Ctrl-N, twice): every route of its OpenAPI specs,
//! grouped by what they are about, each with its method and path, and for
//! the one chosen, all one needs to call it: its parameters, its body with an
//! example, its answers and the errors it may give, a request to copy; then
//! the back end's function that serves it and the front end's files that
//! call it, each a click from open.
//!
//! Flat and painted by egui alone, from the map the plan reads: it costs
//! nothing while still.

use std::collections::BTreeMap;
use std::path::Path;

use bevy_egui::egui::epaint::CubicBezierShape;
use bevy_egui::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Margin, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, text::LayoutJob, vec2,
};
use ironquill_codemap::{CodeMap, Operation};
use ironquill_ui::{App, Effect};

use crate::plan::Plan;
use crate::theme::{
    self, ACCENT, CODE, DIM, EDGE, GREEN, LINK, MAGENTA, PANEL, RED, SELECTED, TEXT, YELLOW,
};

/// The methods to filter by, in the order shown.
const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

/// The most callers drawn in a route's way through; the rest are counted.
const CALLERS_SHOWN: usize = 12;

/// What the API view remembers between frames.
#[derive(Default)]
pub(crate) struct ApiView {
    /// The route chosen, by its place among the map's operations.
    chosen: Option<usize>,
    /// The only method shown, if one is picked.
    method: Option<&'static str>,
}

/// A method's colour, as REST tools tend to give them.
fn method_colour(method: &str) -> Color32 {
    match method {
        "GET" => GREEN,
        "POST" => LINK,
        "PUT" => YELLOW,
        "PATCH" => ACCENT,
        "DELETE" => RED,
        _ => MAGENTA,
    }
}

/// A path with its parameters lit: `/dags/{dag_id}`.
fn path_job(path: &str, size: f32, dim: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    let font = FontId::monospace(size);
    let plain = if dim { DIM } else { TEXT };
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        let close = rest[open..].find('}').map_or(rest.len(), |c| open + c + 1);
        job.append(
            &rest[..open],
            0.0,
            egui::TextFormat::simple(font.clone(), plain),
        );
        job.append(
            &rest[open..close],
            0.0,
            egui::TextFormat::simple(font.clone(), if dim { DIM } else { CODE }),
        );
        rest = &rest[close..];
    }
    job.append(rest, 0.0, egui::TextFormat::simple(font, plain));
    job
}

/// A method in a pill of its colour.
fn method_pill(ui: &mut Ui, method: &str, size: f32) {
    let colour = method_colour(method);
    Frame::new()
        .fill(colour.gamma_multiply(0.16))
        .stroke(Stroke::new(1.0, colour.gamma_multiply(0.6)))
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.set_min_width(size * 3.6);
            ui.label(
                RichText::new(method)
                    .monospace()
                    .size(size)
                    .strong()
                    .color(colour),
            );
        });
}

/// Draws the API in `ui`.
pub(crate) fn show(
    ui: &mut Ui,
    view: &mut ApiView,
    plan: &Plan,
    app: &mut App,
    effects: &mut Vec<Effect>,
) {
    let rect = ui.max_rect();
    let painter = ui.painter_at(rect);
    let project = app.project().to_owned();
    let corner = rect.left_top() + vec2(24.0, 22.0);
    painter.text(
        corner,
        Align2::LEFT_TOP,
        "API",
        FontId::proportional(26.0),
        TEXT,
    );
    crate::plan::switch(ui, rect, app);

    let Some(map) = plan.map() else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Reading the project…",
            FontId::proportional(16.0),
            DIM,
        );
        ui.ctx().request_repaint();
        return;
    };
    let operations = &map.operations;
    if operations.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "No OpenAPI spec found in this project",
            FontId::proportional(16.0),
            DIM,
        );
        return;
    }
    if view.chosen.is_some_and(|c| c >= operations.len()) {
        view.chosen = None;
    }

    // What the API is, under the title.
    let specs = {
        let mut specs: Vec<usize> = operations.iter().map(|o| o.spec).collect();
        specs.sort_unstable();
        specs.dedup();
        specs.len()
    };
    let served = operations.iter().filter(|o| o.handler.is_some()).count();
    let called = operations.iter().filter(|o| !o.callers.is_empty()).count();
    painter.text(
        corner + vec2(0.0, 34.0),
        Align2::LEFT_TOP,
        format!(
            "{project} · {} routes in {specs} spec{} · {served} served · {called} called by the front end",
            operations.len(),
            if specs == 1 { "" } else { "s" },
        ),
        FontId::proportional(13.0),
        DIM,
    );

    // The methods to filter by.
    let chips = Rect::from_min_size(corner + vec2(0.0, 58.0), vec2(520.0, 26.0));
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(chips)
            .layout(egui::Layout::left_to_right(Align::Center)),
        |ui| {
            let all = view.method.is_none();
            if ui
                .add(egui::Button::selectable(
                    all,
                    RichText::new("all")
                        .size(13.0)
                        .color(if all { TEXT } else { DIM }),
                ))
                .clicked()
            {
                view.method = None;
            }
            for method in METHODS {
                let count = operations.iter().filter(|o| o.method == method).count();
                if count == 0 {
                    continue;
                }
                let on = view.method == Some(method);
                let text = RichText::new(format!("{method} {count}"))
                    .monospace()
                    .size(12.5)
                    .color(if on { method_colour(method) } else { DIM });
                if ui.add(egui::Button::selectable(on, text)).clicked() {
                    view.method = if on { None } else { Some(method) };
                }
            }
        },
    );

    let top = rect.top() + 100.0;
    let split = rect.left() + (rect.width() * 0.48).clamp(360.0, 640.0);
    let list = Rect::from_min_max(
        pos2(rect.left() + 16.0, top),
        pos2(split, rect.bottom() - 12.0),
    );
    let detail = Rect::from_min_max(
        pos2(split + 20.0, top),
        pos2(rect.right() - 24.0, rect.bottom() - 12.0),
    );

    ui.scope_builder(UiBuilder::new().max_rect(list), |ui| {
        routes(ui, view, operations);
    });
    // Between the two, a line.
    painter.line_segment(
        [
            pos2(split + 8.0, top),
            pos2(split + 8.0, rect.bottom() - 20.0),
        ],
        Stroke::new(1.0, EDGE),
    );
    ui.scope_builder(UiBuilder::new().max_rect(detail), |ui| match view.chosen {
        Some(chosen) => sheet(ui, map, &operations[chosen], app, plan.root(), effects),
        None => most_called(ui, view, operations),
    });
}

/// Every route, grouped by its first tag, each a row to choose.
fn routes(ui: &mut Ui, view: &mut ApiView, operations: &[Operation]) {
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, operation) in operations.iter().enumerate() {
        if view.method.is_some_and(|m| m != operation.method) {
            continue;
        }
        let tag = operation.tags.first().map_or("untagged", String::as_str);
        groups.entry(tag).or_default().push(i);
    }
    let method_order = |m: &str| {
        METHODS
            .iter()
            .position(|x| *x == m)
            .unwrap_or(METHODS.len())
    };
    ScrollArea::vertical()
        .id_salt("api-routes")
        .auto_shrink(false)
        .show(ui, |ui| {
            for (tag, mut members) in groups {
                members.sort_by(|&a, &b| {
                    let (a, b) = (&operations[a], &operations[b]);
                    a.path
                        .cmp(&b.path)
                        .then(method_order(&a.method).cmp(&method_order(&b.method)))
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tag).size(15.0).strong().color(TEXT));
                    ui.label(
                        RichText::new(members.len().to_string())
                            .size(12.0)
                            .color(DIM),
                    );
                });
                ui.add_space(2.0);
                for i in members {
                    route_row(ui, view, i, &operations[i]);
                }
            }
            ui.add_space(16.0);
        });
}

/// A route in the list: its method, its path, how many call it.
fn route_row(ui: &mut Ui, view: &mut ApiView, index: usize, operation: &Operation) {
    let chosen = view.chosen == Some(index);
    let unserved = operation.handler.is_none();
    let shown = Frame::new()
        .fill(if chosen {
            SELECTED
        } else {
            Color32::TRANSPARENT
        })
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(6, 3))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                method_pill(ui, &operation.method, 11.0);
                ui.add_space(4.0);
                let mut job = path_job(&operation.path, 12.5, unserved || operation.deprecated);
                job.wrap.max_rows = 1;
                job.wrap.max_width = (ui.available_width() - 44.0).max(40.0);
                ui.add(egui::Label::new(job).selectable(false).truncate());
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    let callers = operation.callers.len();
                    if unserved {
                        ui.label(RichText::new("✗").color(RED))
                            .on_hover_text("no function found serving it");
                    } else if callers > 0 {
                        ui.label(RichText::new(callers.to_string()).size(12.0).color(MAGENTA))
                            .on_hover_text("front end files calling it");
                    }
                });
            });
        });
    let response = shown.response.interact(Sense::click());
    if response.hovered() && !chosen {
        ui.painter()
            .rect_filled(response.rect, 6, Color32::from_white_alpha(5));
    }
    let response = response.on_hover_text(if operation.summary.is_empty() {
        operation.id.clone()
    } else {
        operation.summary.clone()
    });
    if response.clicked() {
        view.chosen = if chosen { None } else { Some(index) };
    }
}

/// When no route is chosen: those the front end calls most, as bars.
fn most_called(ui: &mut Ui, view: &mut ApiView, operations: &[Operation]) {
    ui.label(
        RichText::new("Most called by the front end")
            .size(16.0)
            .color(TEXT),
    );
    ui.label(
        RichText::new("click a route for its way through: who calls it, what serves it")
            .size(12.0)
            .color(DIM),
    );
    ui.add_space(10.0);
    let mut ranked: Vec<usize> = (0..operations.len())
        .filter(|&i| !operations[i].callers.is_empty())
        .collect();
    ranked.sort_by(|&a, &b| {
        operations[b]
            .callers
            .len()
            .cmp(&operations[a].callers.len())
            .then(a.cmp(&b))
    });
    let most = ranked
        .first()
        .map_or(1, |&i| operations[i].callers.len())
        .max(1) as f32;
    ScrollArea::vertical()
        .id_salt("api-most")
        .auto_shrink(false)
        .show(ui, |ui| {
            for i in ranked.into_iter().take(24) {
                let operation = &operations[i];
                let shown = ui.horizontal(|ui| {
                    method_pill(ui, &operation.method, 10.5);
                    let mut job = path_job(&operation.path, 12.0, false);
                    job.wrap.max_rows = 1;
                    job.wrap.max_width = (ui.available_width() - 40.0).max(40.0);
                    ui.add(egui::Label::new(job).selectable(false).truncate());
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(operation.callers.len().to_string())
                                .size(12.0)
                                .color(MAGENTA),
                        );
                    });
                });
                let width = ui.available_width();
                let (bar, _) = ui.allocate_exact_size(vec2(width, 5.0), Sense::hover());
                let share = operation.callers.len() as f32 / most;
                ui.painter().rect_filled(bar, 3, theme::see(EDGE, 0.6));
                ui.painter().rect_filled(
                    Rect::from_min_size(bar.min, vec2(bar.width() * share, bar.height())),
                    3,
                    MAGENTA.gamma_multiply(0.8),
                );
                if shown.response.interact(Sense::click()).clicked() {
                    view.chosen = Some(i);
                }
                ui.add_space(6.0);
            }
        });
}

/// A route, as one would call it: what it does, what it takes, what it
/// answers and how it fails, a request to try; then, below, what serves it
/// and what calls it, drawn.
fn sheet(
    ui: &mut Ui,
    map: &CodeMap,
    operation: &Operation,
    app: &mut App,
    root: &Path,
    effects: &mut Vec<Effect>,
) {
    let mut open: Option<(String, Option<usize>)> = None;
    ScrollArea::vertical()
        .id_salt(("api-sheet", &operation.id))
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                method_pill(ui, &operation.method, 14.0);
                ui.add(egui::Label::new(path_job(&operation.path, 16.0, false)).wrap());
            });
            ui.add_space(4.0);
            if !operation.summary.is_empty() {
                ui.label(RichText::new(&operation.summary).size(15.0).color(TEXT));
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(&operation.id)
                        .monospace()
                        .size(12.0)
                        .color(DIM),
                );
                for tag in &operation.tags {
                    ui.label(RichText::new(format!("#{tag}")).size(12.0).color(ACCENT));
                }
                if operation.deprecated {
                    ui.label(RichText::new("deprecated").size(12.0).color(YELLOW));
                }
            });
            if operation.secured {
                ui.label(
                    RichText::new("needs credentials: a token, sent as `Authorization: Bearer …`")
                        .size(12.0)
                        .color(YELLOW),
                );
            }
            ui.label(
                RichText::new(format!("from {}", map.nodes[operation.spec].path))
                    .size(11.5)
                    .color(DIM),
            );
            if !operation.description.is_empty() && operation.description != operation.summary {
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(RichText::new(&operation.description).size(13.0).color(DIM))
                        .wrap(),
                );
            }

            section(ui, "PARAMETERS");
            if operation.parameters.is_empty() {
                ui.label(RichText::new("none").size(12.5).color(DIM));
            }
            for parameter in &operation.parameters {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(&parameter.name)
                            .monospace()
                            .size(13.0)
                            .color(TEXT),
                    );
                    ui.label(
                        RichText::new(&parameter.kind)
                            .monospace()
                            .size(12.0)
                            .color(CODE),
                    );
                    ui.label(
                        RichText::new(format!("in {}", parameter.location))
                            .size(11.5)
                            .color(DIM),
                    );
                    if parameter.required {
                        ui.label(RichText::new("required").size(11.5).color(ACCENT));
                    }
                    if let Some(example) = &parameter.example {
                        ui.label(
                            RichText::new(format!("e.g. {example}"))
                                .size(11.5)
                                .color(DIM),
                        );
                    }
                });
                if !parameter.description.is_empty() {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&parameter.description).size(12.0).color(DIM),
                        )
                        .wrap(),
                    );
                }
                ui.add_space(4.0);
            }

            if let Some(body) = &operation.body {
                section(ui, "REQUEST BODY");
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&body.media).monospace().size(12.0).color(DIM));
                    if let Some(schema) = &body.schema {
                        ui.label(RichText::new(schema).monospace().size(12.5).color(CODE));
                    }
                    if body.required {
                        ui.label(RichText::new("required").size(11.5).color(ACCENT));
                    }
                });
                if let Some(example) = &body.example {
                    code(ui, example, true, effects);
                }
            }

            let errors = operation
                .responses
                .iter()
                .filter(|r| r.status.starts_with('4') || r.status.starts_with('5'))
                .count();
            section(
                ui,
                &format!(
                    "RESPONSES · {} · {errors} error{}",
                    operation.responses.len() - errors,
                    if errors == 1 { "" } else { "s" }
                ),
            );
            for (i, response) in operation.responses.iter().enumerate() {
                let colour = status_colour(&response.status);
                ui.horizontal_wrapped(|ui| {
                    Frame::new()
                        .fill(colour.gamma_multiply(0.16))
                        .stroke(Stroke::new(1.0, colour.gamma_multiply(0.6)))
                        .corner_radius(CornerRadius::same(5))
                        .inner_margin(Margin::symmetric(6, 1))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(&response.status)
                                    .monospace()
                                    .size(12.0)
                                    .strong()
                                    .color(colour),
                            );
                        });
                    ui.label(RichText::new(&response.description).size(13.0).color(TEXT));
                    if let Some(schema) = &response.schema {
                        ui.label(RichText::new(schema).monospace().size(12.0).color(CODE));
                    }
                });
                if let Some(example) = &response.example
                    && response.status.starts_with('2')
                {
                    egui::CollapsingHeader::new(RichText::new("example").size(12.0).color(DIM))
                        .id_salt(("api-response", &operation.id, i))
                        .default_open(false)
                        .show(ui, |ui| code(ui, example, true, effects));
                }
                ui.add_space(3.0);
            }

            section(ui, "EXAMPLE REQUEST");
            code(ui, &operation.curl(), false, effects);

            section(
                ui,
                &format!(
                    "SERVED BY · CALLED BY {} FILE{} OF THE FRONT END",
                    operation.callers.len(),
                    if operation.callers.len() == 1 {
                        ""
                    } else {
                        "S"
                    }
                ),
            );

            // The way through, drawn.
            let callers = &operation.callers;
            let rows = callers.len().clamp(1, CALLERS_SHOWN + 1);
            let row = 30.0;
            let height = (rows as f32 * row).max(3.0 * row) + 10.0;
            let (area, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
            let painter = ui.painter_at(area.expand(4.0));
            // The callers' names need the room; the route says little more than
            // its method there.
            let gap = 28.0;
            let caller_width = (area.width() - 2.0 * gap) * 0.42;
            let route_width = (area.width() - 2.0 * gap) * 0.24;
            let handler_width = area.width() - 2.0 * gap - caller_width - route_width;
            let column = caller_width;
            let left = Rect::from_min_size(area.min, vec2(caller_width, height));
            let middle = Rect::from_center_size(
                pos2(
                    area.left() + caller_width + gap + route_width / 2.0,
                    area.center().y,
                ),
                vec2(route_width, 58.0),
            );
            let right = Rect::from_min_size(
                pos2(area.right() - handler_width, area.center().y - 29.0),
                vec2(handler_width, 58.0),
            );

            // The route, in the middle.
            box_at(&painter, middle, method_colour(&operation.method), 0.12);
            painter.text(
                middle.center() - vec2(0.0, 9.0),
                Align2::CENTER_CENTER,
                &operation.method,
                FontId::monospace(13.0),
                method_colour(&operation.method),
            );
            painter.text(
                middle.center() + vec2(0.0, 10.0),
                Align2::CENTER_CENTER,
                short(&operation.path, route_width - 10.0),
                FontId::monospace(11.0),
                TEXT,
            );

            // What serves it, on the right.
            match operation.handler {
                Some((node, line)) => {
                    let path = &map.nodes[node].path;
                    let response =
                        ui.interact(right, ui.id().with(("handler", node)), Sense::click());
                    box_at(
                        &painter,
                        right,
                        GREEN,
                        if response.hovered() { 0.2 } else { 0.1 },
                    );
                    painter.text(
                        right.left_top() + vec2(10.0, 9.0),
                        Align2::LEFT_TOP,
                        file_name(path),
                        FontId::proportional(13.5),
                        TEXT,
                    );
                    painter.text(
                        right.left_top() + vec2(10.0, 30.0),
                        Align2::LEFT_TOP,
                        format!("line {line} · serves it"),
                        FontId::proportional(11.5),
                        GREEN,
                    );
                    link(
                        &painter,
                        middle.right_center(),
                        right.left_center(),
                        GREEN,
                        2.4,
                    );
                    if response.on_hover_text(format!("{path}:{line}")).clicked() {
                        open = Some((path.clone(), Some(line)));
                    }
                }
                None => {
                    box_at(&painter, right, RED, 0.08);
                    painter.text(
                        right.center(),
                        Align2::CENTER_CENTER,
                        "no function found",
                        FontId::proportional(12.5),
                        RED,
                    );
                }
            }

            // What calls it, on the left.
            if callers.is_empty() {
                painter.text(
                    pos2(left.center().x, area.center().y),
                    Align2::CENTER_CENTER,
                    "no call from the front end",
                    FontId::proportional(12.5),
                    DIM,
                );
            }
            let shown = callers.len().min(CALLERS_SHOWN);
            let first_y = area.center().y - (rows as f32 * row) / 2.0 + row / 2.0;
            for (i, &caller) in callers.iter().take(shown).enumerate() {
                let path = &map.nodes[caller].path;
                let at = Rect::from_center_size(
                    pos2(left.center().x, first_y + i as f32 * row),
                    vec2(column, row - 6.0),
                );
                let response = ui.interact(at, ui.id().with(("caller", caller)), Sense::click());
                box_at(
                    &painter,
                    at,
                    MAGENTA,
                    if response.hovered() { 0.22 } else { 0.08 },
                );
                painter.text(
                    at.left_center() + vec2(8.0, 0.0),
                    Align2::LEFT_CENTER,
                    short(file_name(path), column - 16.0),
                    FontId::proportional(12.0),
                    TEXT,
                );
                link(
                    &painter,
                    at.right_center(),
                    middle.left_center(),
                    MAGENTA,
                    1.4,
                );
                if response.on_hover_text(path.as_str()).clicked() {
                    open = Some((path.clone(), None));
                }
            }
            if callers.len() > shown {
                painter.text(
                    pos2(left.center().x, first_y + shown as f32 * row),
                    Align2::CENTER_CENTER,
                    format!("and {} more", callers.len() - shown),
                    FontId::proportional(12.0),
                    DIM,
                );
            }

            // Every caller, by its way from the root, below.
            if callers.len() > shown {
                ui.add_space(10.0);
                ui.label(RichText::new("ALL THAT CALL IT").size(12.0).color(MAGENTA));
                {
                    {
                        for &caller in callers {
                            let path = &map.nodes[caller].path;
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(path).monospace().size(12.0).color(TEXT),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                open = Some((path.clone(), None));
                            }
                        }
                    }
                }
            }
            ui.add_space(24.0);
        });

    if let Some((path, line)) = open {
        let path = root.join(path);
        match line {
            Some(line) => app.open_path_at(path, line),
            None => app.open_path(path),
        }
        app.show_map(None);
    }
}

/// A section's title.
fn section(ui: &mut Ui, title: &str) {
    ui.add_space(14.0);
    ui.label(RichText::new(title).size(12.0).strong().color(DIM));
    ui.add_space(4.0);
}

/// A status's colour: success green, redirect blue, the caller's fault
/// yellow, the server's red.
fn status_colour(status: &str) -> Color32 {
    match status.chars().next() {
        Some('2') => GREEN,
        Some('3') => LINK,
        Some('4') => YELLOW,
        Some('5') => RED,
        _ => DIM,
    }
}

/// A block of code to read and copy: JSON coloured, keys from values.
fn code(ui: &mut Ui, text: &str, json: bool, effects: &mut Vec<Effect>) {
    Frame::new()
        .fill(theme::RAISED)
        .stroke(Stroke::new(1.0, EDGE))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(Align::Min), |ui| {
                    if ui.small_button(RichText::new("copy").color(DIM)).clicked() {
                        effects.push(Effect::Copy(text.to_owned()));
                    }
                });
            });
            let job = if json {
                json_job(text)
            } else {
                let mut job = LayoutJob::default();
                job.append(
                    text,
                    0.0,
                    egui::TextFormat::simple(FontId::monospace(12.0), TEXT),
                );
                job
            };
            ui.add(egui::Label::new(job).wrap());
        });
}

/// Pretty JSON, coloured: keys blue, strings green, numbers and words
/// yellow, the rest dim.
fn json_job(text: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    let font = FontId::monospace(12.0);
    let mut put = |piece: &str, colour: Color32| {
        if !piece.is_empty() {
            job.append(piece, 0.0, egui::TextFormat::simple(font.clone(), colour));
        }
    };
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix('"') {
            // A string, up to its closing quote, escapes skipped.
            let mut end = stripped.len();
            let mut escaped = false;
            for (i, c) in stripped.char_indices() {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    end = i + 1;
                    break;
                }
            }
            let string = &rest[..=end];
            let after = rest[end + 1..].trim_start();
            put(string, if after.starts_with(':') { LINK } else { GREEN });
            rest = &rest[end + 1..];
        } else {
            let next = rest.find('"').unwrap_or(rest.len());
            let (plain, tail) = rest.split_at(next);
            // Numbers, true, false and null stand out from the punctuation.
            let mut word = String::new();
            for c in plain.chars() {
                if c.is_alphanumeric() || c == '.' || c == '-' {
                    word.push(c);
                } else {
                    put(&word, CODE);
                    word.clear();
                    let mut buffer = [0; 4];
                    put(c.encode_utf8(&mut buffer), DIM);
                }
            }
            put(&word, CODE);
            rest = tail;
        }
    }
    job
}

/// A card of the way through, tinted with `colour`.
fn box_at(painter: &egui::Painter, rect: Rect, colour: Color32, tint: f32) {
    painter.rect(
        rect,
        CornerRadius::same(8),
        mix(theme::see(PANEL, 1.0), colour, tint),
        Stroke::new(1.2, colour.gamma_multiply(0.7)),
        StrokeKind::Inside,
    );
}

/// A glowing curve from `a` to `b`.
fn link(painter: &egui::Painter, a: Pos2, b: Pos2, colour: Color32, width: f32) {
    let bend = vec2((b.x - a.x) * 0.5, 0.0);
    let points = [a, a + bend, b - bend, b];
    for (w, alpha) in [(width * 4.0, 0.08), (width * 2.0, 0.2), (width, 0.9)] {
        painter.add(CubicBezierShape::from_points_stroke(
            points,
            false,
            Color32::TRANSPARENT,
            Stroke::new(w, colour.gamma_multiply(alpha)),
        ));
    }
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let channel = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
    )
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `text` cut to about `width` points, its end kept: a path's end says the
/// most.
fn short(text: &str, width: f32) -> String {
    let fits = (width / 6.6).max(8.0) as usize;
    let count = text.chars().count();
    if count <= fits {
        return text.to_owned();
    }
    let tail: String = text.chars().skip(count - fits + 1).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_path_keeps_its_end() {
        assert_eq!(short("/dags", 200.0), "/dags");
        let cut = short(
            "/api/v2/dags/{dag_id}/dagRuns/{dag_run_id}/taskInstances",
            100.0,
        );
        assert!(cut.starts_with('…') && cut.ends_with("taskInstances"));
        assert_eq!(method_colour("DELETE"), RED);
    }

    #[test]
    fn parameters_are_told_apart_in_a_path() {
        let job = path_job("/dags/{dag_id}/runs", 12.0, false);
        let texts: Vec<&str> = job
            .sections
            .iter()
            .map(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0])
            .collect();
        assert_eq!(texts, ["/dags/", "{dag_id}", "/runs"]);
    }
}
