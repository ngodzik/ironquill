//! The codebase's database (Ctrl-N): the tables its models define, as
//! cards of their columns, in layers by their foreign keys, the tables
//! that point at none at the bottom; each foreign key drawn from its
//! column to the one it points at. A table pointed at lights what it
//! points at and what points at it; a click shows it whole, a double click
//! opens its model.
//!
//! The models are read on a thread of their own, from the files the plan
//! read, by `ironquill-codemap`: never run, and the window never waits.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use bevy_egui::egui::epaint::CubicBezierShape;
use bevy_egui::egui::{
    self, Align2, Color32, CornerRadius, FontId, Frame, Id, Margin, Rect, RichText, ScrollArea,
    Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};
use ironquill_codemap::{Delta, NodeKind, Schema, Table};
use ironquill_ui::App;

use crate::plan::Plan;
use crate::review_view::ReviewView;
use crate::theme::{self, ACCENT, CYAN, DIM, EDGE, PANEL, RAISED, TEXT, YELLOW};

/// A card's width, and the height of its title and of each column's row.
const CARD_WIDTH: f32 = 230.0;
const TITLE: f32 = 34.0;
const ROW: f32 = 17.0;

/// The most columns a card lists; the rest are counted.
const MAX_ROWS: usize = 12;

/// The room between cards.
const GAP_X: f32 = 46.0;
const GAP_Y: f32 = 70.0;

/// The details' width, on the left.
const DETAILS_WIDTH: f32 = 360.0;

/// What the database view remembers between frames.
#[derive(Default)]
pub(crate) struct DatabaseView {
    /// The map the schema was read from, by its size: read again when the
    /// plan reads the project again.
    read_for: Option<usize>,
    reading: Option<Receiver<Schema>>,
    schema: Option<Schema>,
    /// Where each table stands, before panning and zooming, the size of
    /// the whole, and how many cards a row held: as many as make the whole
    /// as wide as the room it is drawn in.
    cards: Vec<Rect>,
    size: Vec2,
    per_row: usize,
    pan: Vec2,
    zoom: f32,
    hovered: Option<usize>,
    chosen: Option<usize>,
}

impl DatabaseView {
    /// Reads the models of the project the plan read, on a thread, once
    /// per reading of the project.
    pub(crate) fn update(&mut self, plan: &Plan) {
        if let Some(map) = plan.map() {
            let key = map.nodes.len();
            if self.read_for != Some(key) {
                self.read_for = Some(key);
                let paths: Vec<String> = map
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.kind, NodeKind::File { .. }))
                    .map(|n| n.path.clone())
                    .filter(|p| models_may_be_in(p))
                    .collect();
                let root = plan.root().to_owned();
                let (send, receive) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = send.send(read(&root, &paths));
                });
                self.reading = Some(receive);
            }
        }
        if let Some(reading) = &self.reading {
            match reading.try_recv() {
                Ok(schema) => {
                    self.per_row = 0;
                    self.schema = Some(schema);
                    self.reading = None;
                    self.chosen = None;
                    if self.zoom == 0.0 {
                        self.zoom = 1.0;
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.reading = None,
            }
        }
    }
}

/// Whether a file may hold models: Python, and neither a test nor a
/// migration, whose tables are the models' history, not the schema.
fn models_may_be_in(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.ends_with(".py")
        && !path.contains("/migrations/")
        && !path.contains("/alembic/")
        && !path.split('/').any(|p| p == "tests" || p == "test")
        && !name.starts_with("test_")
}

/// The schema the models among `paths` define, read from the files that
/// may define a table.
fn read(root: &Path, paths: &[String]) -> Schema {
    let files: Vec<(String, String)> = paths
        .iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(root.join(path)).ok()?;
            let defines = text.contains("__tablename__")
                || text.contains("Table(")
                || text.contains("table=True");
            defines.then(|| (path.clone(), text))
        })
        .collect();
    ironquill_codemap::schema(&files)
}

/// The columns a card lists, by their place in the table: the primary key
/// first, then the foreign keys, then the rest, `MAX_ROWS` at most.
fn listed(table: &Table) -> Vec<usize> {
    let mut order: Vec<usize> = (0..table.columns.len()).collect();
    order.sort_by_key(|&i| {
        let c = &table.columns[i];
        (!c.primary, c.references.is_none(), i)
    });
    order.truncate(MAX_ROWS);
    order
}

/// A card's height: its title, its rows, and one more for those left out.
fn card_height(table: &Table) -> f32 {
    let rows = table.columns.len().min(MAX_ROWS) + usize::from(table.columns.len() > MAX_ROWS);
    TITLE + rows as f32 * ROW + 8.0
}

/// Each table's layer: 0 for one that points at no other, one above the
/// highest of those it points at otherwise; a loop of keys keeps the
/// layer it has when the passes run out.
fn layers(schema: &Schema) -> Vec<usize> {
    let keys = schema.foreign_keys();
    let mut layer = vec![0; schema.tables.len()];
    for _ in 0..schema.tables.len() {
        let mut moved = false;
        for key in keys.iter().filter(|k| k.from != k.to) {
            let wanted = layer[key.to] + 1;
            if layer[key.from] < wanted {
                layer[key.from] = wanted;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    layer
}

/// How many cards a row holds for the whole to be drawn the largest in a
/// room `aspect` times as wide as it is high: each count tried, as layers
/// leave rows part full and no formula foresees how many.
fn per_row(schema: &Schema, aspect: f32) -> usize {
    let aspect = aspect.max(0.1);
    (2..=40)
        .map(|count| {
            let (_, size) = arrange(schema, count);
            // The scale the whole is drawn at in a room of height 1.
            let scale = (aspect / size.x.max(1.0)).min(1.0 / size.y.max(1.0));
            (count, scale)
        })
        .fold((2, 0.0_f32), |best, (count, scale)| {
            if scale > best.1 * 1.0001 {
                (count, scale)
            } else {
                best
            }
        })
        .0
}

/// Where each table stands: layers from the bottom up, each wrapping into
/// rows of `per_row`, a table placed near those it points at. Returns the
/// cards, centred on the origin, and the size of the whole.
fn arrange(schema: &Schema, per_row: usize) -> (Vec<Rect>, Vec2) {
    let count = schema.tables.len();
    if count == 0 {
        return (Vec::new(), Vec2::ZERO);
    }
    let layer = layers(schema);
    let keys = schema.foreign_keys();
    let top = layer.iter().copied().max().unwrap_or(0);
    // The x each table was given, for those above to sit near it.
    let mut x_of = vec![0.0_f32; count];
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for l in 0..=top {
        let mut members: Vec<usize> = (0..count).filter(|t| layer[*t] == l).collect();
        // Near what it points at, else with its file's tables.
        let pull = |t: usize| {
            let targets: Vec<f32> = keys
                .iter()
                .filter(|k| k.from == t && k.to != t && layer[k.to] < l)
                .map(|k| x_of[k.to])
                .collect();
            if targets.is_empty() {
                None
            } else {
                Some(targets.iter().sum::<f32>() / targets.len() as f32)
            }
        };
        members.sort_by(|&a, &b| {
            let (pa, pb) = (pull(a), pull(b));
            match (pa, pb) {
                (Some(x), Some(y)) => x.total_cmp(&y),
                _ => schema.tables[a]
                    .path
                    .cmp(&schema.tables[b].path)
                    .then(schema.tables[a].name.cmp(&schema.tables[b].name)),
            }
        });
        for chunk in members.chunks(per_row.max(1)) {
            let width = chunk.len() as f32 * (CARD_WIDTH + GAP_X) - GAP_X;
            for (i, &t) in chunk.iter().enumerate() {
                x_of[t] = -width / 2.0 + i as f32 * (CARD_WIDTH + GAP_X) + CARD_WIDTH / 2.0;
            }
            rows.push(chunk.to_vec());
        }
    }
    // Bottom up: the foundations' rows last on screen.
    let mut cards = vec![Rect::NOTHING; count];
    let mut y = 0.0_f32;
    let mut widest = 0.0_f32;
    for row in rows.iter() {
        let height = row
            .iter()
            .map(|&t| card_height(&schema.tables[t]))
            .fold(0.0, f32::max);
        y -= height;
        for &t in row {
            let left = x_of[t] - CARD_WIDTH / 2.0;
            cards[t] = Rect::from_min_size(
                pos2(left, y),
                vec2(CARD_WIDTH, card_height(&schema.tables[t])),
            );
            widest = widest.max(x_of[t].abs() + CARD_WIDTH / 2.0);
        }
        y -= GAP_Y;
    }
    let height = -y - GAP_Y;
    // Centred: the middle of the whole at the origin.
    for card in &mut cards {
        *card = card.translate(vec2(0.0, height / 2.0));
    }
    (cards, vec2(widest * 2.0, height))
}

/// A column's type, short enough for a card: `String(512, **ARGS)` as
/// `String(512)`, `Text().with_variant(...)` as `Text`, `Integer()` as
/// `Integer`, nothing for a type the model leaves to its key.
fn short_type(kind: &str) -> String {
    if kind == "?" {
        return String::new();
    }
    let head = kind.split('(').next().unwrap_or(kind);
    let first = kind
        .get(head.len() + 1..)
        .and_then(|rest| rest.split([',', ')']).next())
        .map(str::trim)
        .filter(|arg| !arg.is_empty() && arg.chars().all(|c| c.is_ascii_digit()));
    match first {
        Some(size) => format!("{head}({size})"),
        None => head.to_owned(),
    }
}

/// `text` cut to `most` characters, an ellipsis ending what was cut.
fn cut(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_owned();
    }
    let kept: String = text.chars().take(most.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// What the branch looked at changed in the database, by table: the
/// table itself, or some of its columns by name.
fn changed(review: &ReviewView) -> Vec<(String, Delta, Option<String>)> {
    let Some(read) = review.review() else {
        return Vec::new();
    };
    let steps = read
        .models
        .iter()
        .flat_map(|(_, changes)| changes.iter())
        .chain(read.migrations.iter().flat_map(|(_, m)| m.steps.iter()));
    steps
        .filter_map(|step| {
            let table = step.table.clone()?;
            let column = step
                .what
                .strip_prefix("column ")
                .map(|c| c.split_whitespace().next().unwrap_or(c).to_owned());
            Some((table, step.delta, column))
        })
        .collect()
}

/// Draws the database in `ui`.
pub(crate) fn show(ui: &mut Ui, view: &mut DatabaseView, review: &ReviewView, app: &mut App) {
    let rect = ui.max_rect();
    let response = ui.allocate_rect(
        rect.with_max_x(rect.max.x - crate::plan::EDGE_GRIP),
        Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    let corner = rect.left_top() + vec2(24.0, 22.0);
    painter.text(
        corner,
        Align2::LEFT_TOP,
        "Database",
        FontId::proportional(26.0),
        TEXT,
    );

    let Some(schema) = view.schema.clone() else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Reading the models…",
            FontId::proportional(16.0),
            DIM,
        );
        ui.ctx().request_repaint();
        crate::plan::switch(ui, rect, app);
        return;
    };
    if schema.tables.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "No SQLAlchemy or SQLModel table found in this project",
            FontId::proportional(16.0),
            DIM,
        );
        crate::plan::switch(ui, rect, app);
        return;
    }
    let keys = schema.foreign_keys();
    let files: HashSet<&str> = schema.tables.iter().map(|t| t.path.as_str()).collect();
    let partly = schema.tables.iter().filter(|t| t.partly.is_some()).count();
    let mut summary = format!(
        "{} · {} tables · {} foreign keys · from {} files",
        app.project(),
        schema.tables.len(),
        keys.len(),
        files.len()
    );
    if partly > 0 {
        summary.push_str(&format!(" · {partly} read in part"));
    }
    painter.text(
        corner + vec2(0.0, 34.0),
        Align2::LEFT_TOP,
        summary,
        FontId::proportional(13.0),
        DIM,
    );

    // The changes of the branch looked at, and what the search keeps.
    let touched = changed(review);
    let table_changed = |name: &str| {
        touched
            .iter()
            .find(|(t, _, _)| t == name)
            .map(|(_, d, _)| *d)
    };
    let column_changed = |table: &str, column: &str| {
        touched
            .iter()
            .any(|(t, _, c)| t == table && c.as_deref() == Some(column))
    };
    let query = app.map_search().to_owned();
    let kept: Vec<bool> = schema
        .tables
        .iter()
        .map(|t| {
            let searched = query.is_empty()
                || ironquill_ui::search_matches(
                    &query,
                    [
                        t.name.as_str(),
                        t.model.as_deref().unwrap_or(""),
                        t.path.as_str(),
                    ]
                    .into_iter()
                    .chain(t.columns.iter().map(|c| c.name.as_str())),
                );
            let reviewing = app.change_set().is_some() && !touched.is_empty();
            searched && (!reviewing || table_changed(&t.name).is_some())
        })
        .collect();

    // Room for the title, and for the details when a table is chosen.
    let aside = if view.chosen.is_some() {
        DETAILS_WIDTH + 24.0
    } else {
        0.0
    };
    let room = Rect::from_min_max(
        rect.min + vec2(40.0 + aside, 96.0),
        rect.max - vec2(40.0, 40.0),
    );
    // Laid out again when the room's shape asks for other rows; not when
    // a table is chosen, which narrows it for a moment.
    let full = Rect::from_min_max(rect.min + vec2(40.0, 96.0), rect.max - vec2(40.0, 40.0));
    let wanted = per_row(&schema, full.width() / full.height().max(1.0));
    if wanted != view.per_row {
        view.per_row = wanted;
        (view.cards, view.size) = arrange(&schema, wanted);
    }
    let fit = (room.width() / view.size.x.max(1.0))
        .min(room.height() / view.size.y.max(1.0))
        .clamp(0.2, 1.2);
    if response.dragged() {
        view.pan += response.drag_delta();
    }
    if let Some(pointer) = response.hover_pos() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            let before = view.zoom.max(0.1);
            view.zoom = (before * (scroll * 0.0018).exp()).clamp(0.3, 5.0);
            let from_centre = pointer - room.center() - view.pan;
            view.pan += from_centre - from_centre * (view.zoom / before);
        }
    }
    let scale = fit * view.zoom.max(0.1);
    let centre = room.center() + view.pan;
    let screen = |r: Rect| {
        Rect::from_min_max(
            centre + r.min.to_vec2() * scale,
            centre + r.max.to_vec2() * scale,
        )
    };
    let cards: Vec<Rect> = view.cards.iter().map(|r| screen(*r)).collect();
    view.hovered = response
        .hover_pos()
        .and_then(|p| cards.iter().position(|c| c.contains(p)));
    if response.double_clicked()
        && let Some(t) = view.hovered
    {
        let table = &schema.tables[t];
        let path: PathBuf = app.root().join(&table.path);
        app.show_map(None);
        app.open_path_at(path, table.line);
        return;
    }
    if response.clicked() {
        view.chosen = view.hovered;
    }
    let focus = view.hovered.or(view.chosen);
    let rows: Vec<Vec<usize>> = schema.tables.iter().map(listed).collect();

    // Under the title and the switch, whatever the zoom.
    let canvas = painter.with_clip_rect(Rect::from_min_max(
        pos2(rect.left(), rect.top() + 92.0),
        rect.max,
    ));
    // The foreign keys, under the cards: from the column that points, on
    // the side facing what it points at.
    let row_y = |t: usize, column: Option<usize>| {
        let card = cards[t];
        match column.and_then(|c| rows[t].iter().position(|r| *r == c)) {
            Some(row) => card.top() + (TITLE + (row as f32 + 0.5) * ROW) * scale,
            None => card.top() + TITLE * 0.5 * scale,
        }
    };
    for key in &keys {
        if key.from == key.to {
            continue;
        }
        let (a, b) = (cards[key.from], cards[key.to]);
        let rightward = b.center().x >= a.center().x;
        let start = pos2(
            if rightward { a.right() } else { a.left() },
            row_y(key.from, Some(key.from_column)),
        );
        let end = pos2(
            if rightward { b.left() } else { b.right() },
            row_y(key.to, key.to_column),
        );
        let bend = ((end.x - start.x).abs() * 0.5).max(40.0 * scale);
        let sign = if rightward { 1.0 } else { -1.0 };
        let points = [
            start,
            start + vec2(bend * sign, 0.0),
            end - vec2(bend * sign, 0.0),
            end,
        ];
        let (colour, alpha, width) = match focus {
            Some(f) if f == key.from => (ACCENT, 0.95, 2.0),
            Some(f) if f == key.to => (CYAN, 0.95, 2.0),
            Some(_) => (CYAN, 0.06, 1.0),
            None if kept[key.from] && kept[key.to] => (CYAN, 0.35, 1.2),
            None => (CYAN, 0.06, 1.0),
        };
        canvas.add(CubicBezierShape::from_points_stroke(
            points,
            false,
            Color32::TRANSPARENT,
            Stroke::new(width * scale.sqrt(), colour.gamma_multiply(alpha)),
        ));
        canvas.circle_filled(end, 2.5 * scale.sqrt(), colour.gamma_multiply(alpha));
    }

    // The cards.
    let related = |t: usize| {
        focus.is_none_or(|f| {
            f == t
                || keys
                    .iter()
                    .any(|k| (k.from == f && k.to == t) || (k.to == f && k.from == t))
        })
    };
    for (t, table) in schema.tables.iter().enumerate() {
        let card = cards[t];
        if !card.intersects(rect) {
            continue;
        }
        let alpha: f32 = if !kept[t] {
            0.18
        } else if related(t) {
            1.0
        } else {
            0.3
        };
        let picked = focus == Some(t);
        let delta = table_changed(&table.name);
        let edge = match delta {
            Some(Delta::Added) => theme::GREEN,
            Some(Delta::Removed) => theme::RED,
            Some(Delta::Changed) => YELLOW,
            None if picked => ACCENT,
            None => EDGE,
        };
        canvas.rect(
            card,
            CornerRadius::same((8.0 * scale.min(1.4)) as u8),
            theme::see(RAISED, alpha.max(0.5)),
            Stroke::new(if picked { 1.8 } else { 1.0 }, edge.gamma_multiply(alpha)),
            StrokeKind::Inside,
        );
        let text = |size: f32| FontId::proportional(size * scale);
        let mono = |size: f32| FontId::monospace(size * scale);
        // Its name stays readable from afar, cut to the card's width.
        let title = (14.0 * scale).max(10.0);
        let room_chars = ((card.width() - 12.0) / (title * 0.55)).max(3.0) as usize;
        canvas.text(
            card.left_top() + vec2(10.0 * scale, 7.0 * scale),
            Align2::LEFT_TOP,
            cut(&table.name, room_chars),
            FontId::proportional(title),
            TEXT.gamma_multiply(alpha),
        );
        // Its model's name, when it fits beside its own.
        let fits = |chars: usize, size: f32| chars as f32 * size * 0.55 * scale;
        if let Some(model) = table.model.as_ref().filter(|m| {
            fits(table.name.len(), 14.0) + fits(m.len(), 10.5) + 30.0 * scale < card.width()
        }) {
            canvas.text(
                card.right_top() + vec2(-10.0, 9.0) * scale,
                Align2::RIGHT_TOP,
                model,
                text(10.5),
                DIM.gamma_multiply(alpha),
            );
        }
        // Too small to read, the columns are left out.
        if scale < 0.45 {
            continue;
        }
        for (row, &c) in rows[t].iter().enumerate() {
            let column = &table.columns[c];
            let y = card.top() + (TITLE + row as f32 * ROW) * scale;
            if column_changed(&table.name, &column.name) {
                canvas.rect_filled(
                    Rect::from_min_size(
                        pos2(card.left() + 2.0, y),
                        vec2(card.width() - 4.0, ROW * scale),
                    ),
                    CornerRadius::same(3),
                    YELLOW.gamma_multiply(0.12 * alpha),
                );
            }
            let (mark, mark_colour) = if column.primary {
                ("◆", YELLOW)
            } else if column.references.is_some() {
                ("→", CYAN)
            } else {
                ("·", DIM)
            };
            canvas.text(
                pos2(card.left() + 10.0 * scale, y + ROW * 0.5 * scale),
                Align2::LEFT_CENTER,
                mark,
                mono(11.0),
                mark_colour.gamma_multiply(alpha),
            );
            // The type keeps its room; the name is cut short before it.
            let kind = short_type(&column.kind);
            let kind = format!(
                "{kind}{}",
                if column.nullable && !kind.is_empty() {
                    "?"
                } else {
                    ""
                }
            );
            let glyph = 0.6 * scale;
            let room = card.width() - 34.0 * scale - kind.chars().count() as f32 * 10.0 * glyph;
            let name = cut(&column.name, (room / (11.0 * glyph)).max(3.0) as usize);
            canvas.text(
                pos2(card.left() + 24.0 * scale, y + ROW * 0.5 * scale),
                Align2::LEFT_CENTER,
                name,
                mono(11.0),
                TEXT.gamma_multiply(alpha * if column.nullable { 0.75 } else { 1.0 }),
            );
            canvas.text(
                pos2(card.right() - 10.0 * scale, y + ROW * 0.5 * scale),
                Align2::RIGHT_CENTER,
                kind,
                mono(10.0),
                DIM.gamma_multiply(alpha),
            );
        }
        if table.columns.len() > MAX_ROWS {
            let y = card.top() + (TITLE + MAX_ROWS as f32 * ROW) * scale;
            canvas.text(
                pos2(card.left() + 24.0 * scale, y + ROW * 0.5 * scale),
                Align2::LEFT_CENTER,
                format!("and {} more", table.columns.len() - MAX_ROWS),
                text(10.5),
                DIM.gamma_multiply(alpha),
            );
        }
    }

    painter.text(
        rect.left_bottom() + vec2(24.0, -18.0),
        Align2::LEFT_BOTTOM,
        "point at a table for its keys · click it for its columns · double-click to open its model · drag to move · scroll to zoom · ◆ primary key · → foreign key · ? nullable",
        FontId::proportional(12.0),
        DIM,
    );
    if let Some(chosen) = view.chosen {
        details(ui, rect, view, &schema, chosen, app);
    }
    crate::plan::switch(ui, rect, app);
}

/// The chosen table whole: its columns, its indexes, what it points at and
/// what points at it, each a click away.
fn details(
    ui: &mut Ui,
    rect: Rect,
    view: &mut DatabaseView,
    schema: &Schema,
    chosen: usize,
    app: &mut App,
) {
    let table = &schema.tables[chosen];
    let keys = schema.foreign_keys();
    let top = rect.left_top() + vec2(24.0, 96.0);
    let height = (rect.bottom() - 60.0 - top.y).max(120.0);
    let mut choose = None;
    let mut open = false;
    let mut close = false;
    egui::Area::new(Id::new("database-details"))
        .fixed_pos(top)
        .show(ui.ctx(), |ui| {
            Frame::new()
                .fill(theme::see(PANEL, 0.95))
                .stroke(Stroke::new(1.0, EDGE))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(14))
                .show(ui, |ui| {
                    ui.set_width(DETAILS_WIDTH - 28.0);
                    ui.set_max_height(height);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&table.name).size(19.0).color(TEXT));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close = ui.small_button("close").clicked();
                            open = ui
                                .small_button("open")
                                .on_hover_text("open its model")
                                .clicked();
                        });
                    });
                    ui.label(
                        RichText::new(format!(
                            "{}{}:{}",
                            table
                                .model
                                .as_ref()
                                .map(|m| format!("{m} · "))
                                .unwrap_or_default(),
                            table.path,
                            table.line
                        ))
                        .size(11.5)
                        .color(DIM),
                    );
                    if let Some(why) = &table.partly {
                        ui.label(
                            RichText::new(format!("read in part: {why}"))
                                .size(11.5)
                                .color(YELLOW),
                        );
                    }
                    ScrollArea::vertical()
                        .id_salt("database-details")
                        .show(ui, |ui| {
                            ui.add_space(8.0);
                            ui.label(RichText::new("COLUMNS").size(11.0).color(DIM));
                            for column in &table.columns {
                                ui.horizontal(|ui| {
                                    let mark = if column.primary {
                                        "◆"
                                    } else if column.references.is_some() {
                                        "→"
                                    } else {
                                        "·"
                                    };
                                    ui.label(
                                        RichText::new(mark).monospace().color(if column.primary {
                                            YELLOW
                                        } else {
                                            CYAN
                                        }),
                                    );
                                    ui.label(RichText::new(&column.name).monospace().color(TEXT));
                                    ui.label(
                                        RichText::new(&column.kind)
                                            .monospace()
                                            .size(11.5)
                                            .color(DIM),
                                    );
                                    let mut flags = Vec::new();
                                    if column.nullable {
                                        flags.push("null");
                                    }
                                    if column.unique {
                                        flags.push("unique");
                                    }
                                    if column.indexed {
                                        flags.push("index");
                                    }
                                    if !flags.is_empty() {
                                        ui.label(
                                            RichText::new(flags.join(" ")).size(11.0).color(DIM),
                                        );
                                    }
                                });
                                if let Some(reference) = &column.references {
                                    ui.horizontal(|ui| {
                                        ui.add_space(22.0);
                                        let target =
                                            format!("→ {}.{}", reference.table, reference.column);
                                        let link = ui.add(
                                            egui::Label::new(
                                                RichText::new(target).size(11.5).color(CYAN),
                                            )
                                            .sense(Sense::click()),
                                        );
                                        if link
                                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                                            .clicked()
                                        {
                                            choose = schema.table(&reference.table);
                                        }
                                    });
                                }
                            }
                            if !table.indexes.is_empty() {
                                ui.add_space(8.0);
                                ui.label(RichText::new("INDEXES").size(11.0).color(DIM));
                                for index in &table.indexes {
                                    ui.label(
                                        RichText::new(format!(
                                            "{}{} ({})",
                                            if index.unique { "unique " } else { "" },
                                            index.name,
                                            index.columns.join(", ")
                                        ))
                                        .size(12.0)
                                        .color(TEXT),
                                    );
                                }
                            }
                            let pointed_at_by: Vec<usize> = {
                                let mut from: Vec<usize> = keys
                                    .iter()
                                    .filter(|k| k.to == chosen && k.from != chosen)
                                    .map(|k| k.from)
                                    .collect();
                                from.sort_unstable();
                                from.dedup();
                                from
                            };
                            if !pointed_at_by.is_empty() {
                                ui.add_space(8.0);
                                ui.label(RichText::new("POINTED AT BY").size(11.0).color(DIM));
                                for from in pointed_at_by {
                                    let link = ui.add(
                                        egui::Label::new(
                                            RichText::new(&schema.tables[from].name)
                                                .size(12.5)
                                                .color(ACCENT),
                                        )
                                        .sense(Sense::click()),
                                    );
                                    if link
                                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                                        .clicked()
                                    {
                                        choose = Some(from);
                                    }
                                }
                            }
                        });
                });
        });
    if close {
        view.chosen = None;
    }
    if let Some(next) = choose {
        view.chosen = Some(next);
    }
    if open {
        let path = app.root().join(&table.path);
        app.show_map(None);
        app.open_path_at(path, table.line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(text: &str) -> Schema {
        ironquill_codemap::schema(&[("models.py".to_owned(), text.to_owned())])
    }

    #[test]
    fn tables_pointed_at_stand_below_those_that_point_at_them() {
        let text = r#"
class Dag(Base):
    __tablename__ = "dag"
    dag_id = Column(String, primary_key=True)

class DagRun(Base):
    __tablename__ = "dag_run"
    id = Column(Integer, primary_key=True)
    dag_id = Column(String, ForeignKey("dag.dag_id"))

class TaskInstance(Base):
    __tablename__ = "task_instance"
    id = Column(Integer, primary_key=True)
    run_id = Column(Integer, ForeignKey("dag_run.id"))

class Log(Base):
    __tablename__ = "log"
    id = Column(Integer, primary_key=True)
"#;
        let schema = schema(text);
        let names: Vec<&str> = schema.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["dag", "dag_run", "log", "task_instance"]);
        assert_eq!(layers(&schema), [0, 1, 0, 2]);
        let (cards, size) = arrange(&schema, 7);
        // Higher on screen is lower y: the foundations at the bottom.
        let y = |t: usize| cards[t].center().y;
        assert!(y(3) < y(1) && y(1) < y(0));
        assert!(
            (y(0) - y(2)).abs() < 1.0,
            "dag and log share the foundations"
        );
        // Nothing overlaps.
        for a in 0..cards.len() {
            for b in a + 1..cards.len() {
                assert!(!cards[a].intersects(cards[b]), "{a} and {b} overlap");
            }
        }
        assert!(size.x > 0.0 && size.y > 0.0);
        // A wide room holds more cards a row than a tall one.
        assert!(per_row(&schema, 3.0) >= per_row(&schema, 0.3));
    }

    #[test]
    fn a_card_lists_its_keys_first_and_counts_what_it_leaves_out() {
        let mut columns = String::new();
        for i in 0..15 {
            columns.push_str(&format!("    c{i} = Column(Integer)\n"));
        }
        let text = format!(
            "class Big(Base):\n    __tablename__ = \"big\"\n{columns}    parent = Column(Integer, ForeignKey(\"big.c0\"))\n    id = Column(Integer, primary_key=True)\n"
        );
        let schema = schema(&text);
        let table = &schema.tables[0];
        let rows = listed(table);
        assert_eq!(rows.len(), MAX_ROWS);
        assert_eq!(table.columns[rows[0]].name, "id");
        assert_eq!(table.columns[rows[1]].name, "parent");
        assert!(card_height(table) > TITLE + MAX_ROWS as f32 * ROW);
        assert_eq!(short_type("String(512, **COLLATION_ARGS)"), "String(512)");
        assert_eq!(
            short_type("Text().with_variant(MEDIUMTEXT(), \"mysql\")"),
            "Text"
        );
        assert_eq!(short_type("StringID()"), "StringID");
        assert_eq!(short_type("UtcDateTime"), "UtcDateTime");
        assert_eq!(short_type("?"), "");
        assert_eq!(cut("exceeds_max_non_backfill", 10), "exceeds_m…");
        assert_eq!(cut("dag_id", 10), "dag_id");
        assert!(models_may_be_in("airflow/models/dag.py"));
        assert!(!models_may_be_in("airflow/migrations/versions/0001_x.py"));
        assert!(!models_may_be_in("tests/models/test_dag.py"));
    }
}
