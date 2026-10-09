//! The codebase's plan (Ctrl-N): its components as cards in layers, the
//! foundations at the bottom, each link what one uses of another, thicker
//! for more imports, red when it closes a loop. Pointing at a card lights
//! what it uses and what uses it; clicking one lists its files. A card
//! ripples when the agent reads one of its files, and flares when it edits
//! one.
//!
//! Flat and painted by egui alone: it costs nothing while still, and only
//! asks for frames while something on it moves.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use bevy_egui::egui::epaint::{CubicBezierShape, Shadow};
use bevy_egui::egui::{
    self, Align2, Color32, CornerRadius, FontId, Frame, Id, Margin, Painter, Pos2, Rect, RichText,
    ScrollArea, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};
use ironquill_codemap::{Architecture, CodeMap, ComponentKind};
use ironquill_ui::{App, Entry, MapView};

use crate::theme::{self, ACCENT, CYAN, DIM, EDGE, PANEL, RAISED, RED, TEXT};

/// The most files mapped: the code first, so that a monorepo's shows
/// whole; read away from the window, which shows the last plan meanwhile.
const MAX_FILES: usize = 40_000;

/// The most cards in a row: a layer with more wraps onto more rows.
const ROW_CARDS: usize = 8;

/// A card's height, the least width, and the room around it, at a zoom
/// of 1.
const CARD_HEIGHT: f32 = 68.0;
const CARD_WIDTH: f32 = 150.0;
const ROW_GAP: f32 = 78.0;
const COLUMN_GAP: f32 = 34.0;
/// Between the rows of one layer that wraps.
const WRAP_GAP: f32 = 26.0;

/// How long a read and an edit by the agent show on a card, in seconds.
const READ_SHOWS: f64 = 1.8;
const EDIT_SHOWS: f64 = 3.2;

/// How long the cards take to rise into place when the plan opens, and how
/// much later each layer starts than the one under it.
const RISE: f64 = 0.45;
const RISE_STAGGER: f64 = 0.09;

/// How far from the conversation's border dragging leaves the plan alone:
/// the border's own grip reaches that far into the plan.
pub(crate) const EDGE_GRIP: f32 = 8.0;

/// How long a second click may wait to make a double click, in seconds:
/// egui's own.
const DOUBLE_CLICK: f64 = 0.3;

/// The width of the card listing a chosen component's files.
const DETAILS_WIDTH: f32 = 340.0;

/// What the agent last did in a component.
struct Touch {
    at: f64,
    edited: bool,
    file: String,
}

/// The plan and how it is looked at.
pub(crate) struct Plan {
    root: PathBuf,
    map: Option<CodeMap>,
    design: Option<Architecture>,
    /// The project being read again, on a thread of its own.
    reading: Option<Receiver<CodeMap>>,
    /// The folder whose parts show: opened by a double click.
    scope: String,
    /// Whether it showed last frame: opening it reads the project again.
    shown: bool,
    /// When it opened, for the cards to rise.
    opened_at: f64,
    /// How many transcript entries were looked at for the agent's work.
    seen: usize,
    touches: Vec<Option<Touch>>,
    /// Moved and scaled by the mouse, from the fit to the window.
    pan: Vec2,
    zoom: f32,
    hovered: Option<usize>,
    chosen: Option<usize>,
    /// When the part was chosen: its files show a double click's time
    /// later, so that the card they show on does not take the second click.
    chosen_at: f64,
}

impl Plan {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            map: None,
            design: None,
            reading: None,
            scope: String::new(),
            shown: false,
            opened_at: 0.0,
            seen: 0,
            touches: Vec::new(),
            pan: Vec2::ZERO,
            zoom: 1.0,
            hovered: None,
            chosen: None,
            chosen_at: 0.0,
        }
    }

    /// Notes whether the plan shows this frame. Each time it opens, the
    /// project is read again, as the agent may have changed it, and what
    /// the agent did before is not replayed.
    pub(crate) fn showing(&mut self, shown: bool, now: f64, transcript: &[Entry]) {
        if shown && !self.shown && self.reading.is_none() {
            let (send, receive) = mpsc::channel();
            let root = self.root.clone();
            std::thread::spawn(move || {
                // The plan may be gone by then: nothing to tell.
                let _ = send.send(ironquill_codemap::map(&root, MAX_FILES));
            });
            self.reading = Some(receive);
            self.opened_at = now;
            self.seen = transcript.len();
        }
        self.shown = shown;
    }

    /// Takes the project read, once read, and draws its plan at the level
    /// shown.
    pub(crate) fn take_read(&mut self, now: f64) {
        let Some(reading) = &self.reading else {
            return;
        };
        match reading.try_recv() {
            Ok(map) => {
                self.reading = None;
                self.map = Some(map);
                self.redesign(now);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.reading = None,
        }
    }

    /// Whether the project is being read.
    pub(crate) fn is_reading(&self) -> bool {
        self.reading.is_some()
    }

    /// Shows the parts of `folder`, or of the whole with `""`.
    pub(crate) fn open(&mut self, folder: &str, now: f64) {
        self.scope = folder.to_owned();
        self.chosen = None;
        self.hovered = None;
        self.pan = Vec2::ZERO;
        self.zoom = 1.0;
        self.redesign(now);
    }

    /// Draws the design again at the level shown, from the project read.
    fn redesign(&mut self, now: f64) {
        let Some(map) = &self.map else {
            return;
        };
        let mut design = ironquill_codemap::architecture(map, &self.scope);
        // A folder gone since: the whole instead.
        if design.components.is_empty() && !self.scope.is_empty() {
            self.scope.clear();
            design = ironquill_codemap::architecture(map, "");
        }
        self.touches = (0..design.components.len()).map(|_| None).collect();
        if self.chosen.is_some_and(|c| c >= design.components.len()) {
            self.chosen = None;
        }
        self.design = Some(design);
        self.opened_at = now;
    }

    /// Marks the components whose files the agent read or edited since
    /// last frame.
    pub(crate) fn light_up(&mut self, transcript: &[Entry], now: f64) {
        let from = self.seen.min(transcript.len());
        self.seen = transcript.len();
        let (Some(map), Some(design)) = (&self.map, &self.design) else {
            return;
        };
        for (path, edited) in crate::activity::touched(&transcript[from..]) {
            let Some(component) = map.find(path).and_then(|n| design.owner(n)) else {
                continue;
            };
            // A read does not hide an edit still showing.
            let slot = &mut self.touches[component];
            if !edited
                && slot
                    .as_ref()
                    .is_some_and(|t| t.edited && now - t.at < EDIT_SHOWS)
            {
                continue;
            }
            *slot = Some(Touch {
                at: now,
                edited,
                file: path.rsplit('/').next().unwrap_or(path).to_owned(),
            });
        }
    }
}

/// Where each component's card is, in the plan's own space, and how big
/// the whole is.
fn arrange(painter: &Painter, design: &Architecture, project: &str) -> (Vec<Rect>, Vec2) {
    let mut cards = vec![Rect::NOTHING; design.components.len()];
    let mut widest: f32 = 0.0;
    let rows: Vec<Vec<(usize, f32)>> = design
        .layers
        .iter()
        .map(|layer| {
            layer
                .iter()
                .map(|&c| {
                    let component = &design.components[c];
                    let name = painter
                        .layout_no_wrap(title(component, project).to_owned(), name_font(1.0), TEXT)
                        .size()
                        .x;
                    let sub = painter
                        .layout_no_wrap(subtitle(component), small_font(1.0), DIM)
                        .size()
                        .x;
                    (c, (name.max(sub) + 40.0).max(CARD_WIDTH))
                })
                .collect()
        })
        .collect();
    // From the top layer down, the foundations at the bottom; a layer of
    // many cards wraps onto rows of its own.
    let mut y = 0.0;
    for row in rows.iter().rev() {
        let lines: Vec<&[(usize, f32)]> = row.chunks(ROW_CARDS).collect();
        for (i, line) in lines.iter().enumerate() {
            let width: f32 = line.iter().map(|(_, w)| w).sum::<f32>()
                + COLUMN_GAP * line.len().saturating_sub(1) as f32;
            widest = widest.max(width);
            let mut x = -width / 2.0;
            for &(c, w) in *line {
                cards[c] = Rect::from_min_size(pos2(x, y), vec2(w, CARD_HEIGHT));
                x += w + COLUMN_GAP;
            }
            y += CARD_HEIGHT
                + if i + 1 < lines.len() {
                    WRAP_GAP
                } else {
                    ROW_GAP
                };
        }
    }
    let tall = (y - ROW_GAP).max(0.0);
    // Centred on the origin.
    for card in &mut cards {
        *card = card.translate(vec2(0.0, -tall / 2.0));
    }
    (cards, vec2(widest, tall))
}

fn title<'a>(component: &'a ironquill_codemap::Component, project: &'a str) -> &'a str {
    if component.name.is_empty() {
        project
    } else {
        &component.name
    }
}

fn subtitle(component: &ironquill_codemap::Component) -> String {
    let files = component.files.len();
    let size = format!(
        "{} file{} · {} lines",
        thousands(files),
        if files == 1 { "" } else { "s" },
        thousands(component.lines)
    );
    match component.kind {
        ComponentKind::Group { packages } => format!("{packages} packages · {size}"),
        _ => size,
    }
}

/// Whether a part opens onto parts of its own.
fn opens(component: &ironquill_codemap::Component) -> bool {
    component.kind != ComponentKind::File
}

/// `1234` as `1.2k`: a card's size at a glance.
fn thousands(n: usize) -> String {
    if n >= 1_000 {
        format!("{:.1}k", n as f32 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn name_font(scale: f32) -> FontId {
    FontId::proportional(text_size(16.0 * scale))
}

fn small_font(scale: f32) -> FontId {
    FontId::proportional(text_size(12.0 * scale))
}

/// The sizes text is drawn at as the plan zooms. egui draws each size's
/// letters into one texture, and makes it anew, whole, once full: a size
/// for every step of the wheel filled it within a few turns, and the frame
/// it is made anew in is drawn empty, a flash of what is behind.
const TEXT_SIZES: [f32; 12] = [
    7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 14.0, 16.0, 19.0, 22.0, 26.0, 30.0,
];

/// The size of `TEXT_SIZES` nearest to `size`.
fn text_size(size: f32) -> f32 {
    TEXT_SIZES
        .iter()
        .copied()
        .min_by(|a, b| (a - size).abs().total_cmp(&(b - size).abs()))
        .unwrap_or(size)
}

/// A layer's colour: cool for the foundations, warm for what sits on top.
fn layer_colour(layer: usize, layers: usize) -> Color32 {
    let t = if layers > 1 {
        layer as f32 / (layers - 1) as f32
    } else {
        0.0
    };
    mix(CYAN, ACCENT, t)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
    )
}

fn faded(colour: Color32, alpha: f32) -> Color32 {
    colour.gamma_multiply(alpha.clamp(0.0, 1.0))
}

/// A point of the cubic Bézier `p` at `t`.
fn bezier(p: [Pos2; 4], t: f32) -> Pos2 {
    let u = 1.0 - t;
    let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    pos2(
        p.iter().zip(w).map(|(p, w)| p.x * w).sum(),
        p.iter().zip(w).map(|(p, w)| p.y * w).sum(),
    )
}

/// Where the Bézier `p`, going down, is at height `y`: found by halving,
/// as its height only grows along it.
fn at_height(p: [Pos2; 4], y: f32) -> f32 {
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..24 {
        let mid = (low + high) / 2.0;
        if bezier(p, mid).y < y {
            low = mid;
        } else {
            high = mid;
        }
    }
    (low + high) / 2.0
}

/// Draws the plan in `ui`, and handles the mouse over it.
pub(crate) fn show(ui: &mut Ui, plan: &mut Plan, app: &mut App) {
    let rect = ui.max_rect();
    // Short of the right edge, where the conversation's border is dragged.
    let response = ui.allocate_rect(
        rect.with_max_x(rect.max.x - EDGE_GRIP),
        Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    let now = ui.input(|i| i.time);
    let project = app.project().to_owned();
    let mut moving = false;

    if plan.is_reading() {
        // Something moves: the window draws until the project is read.
        moving = true;
        let dots = ".".repeat(1 + (now * 3.0) as usize % 3);
        painter.text(
            rect.center_bottom() - vec2(0.0, 70.0),
            Align2::CENTER_CENTER,
            format!("Reading the project{dots}"),
            FontId::proportional(15.0),
            DIM,
        );
    }
    let Some(design) = plan.design.as_ref() else {
        if moving {
            ui.ctx().request_repaint();
        }
        return;
    };
    if design.components.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "No source code found to draw a plan of",
            FontId::proportional(18.0),
            DIM,
        );
        if let Some(folder) = header(ui, &painter, rect, app, design, &project) {
            plan.open(&folder, now);
        }
        return;
    }

    // Room for the title above and, when a component is chosen, for its
    // files on the left: the plan slides aside rather than under it.
    let aside = ui.ctx().animate_value_with_time(
        Id::new("plan-aside"),
        if plan.chosen.is_some() && now - plan.chosen_at > DOUBLE_CLICK {
            DETAILS_WIDTH + 24.0
        } else {
            0.0
        },
        0.25,
    );
    let room = Rect::from_min_max(
        rect.min + vec2(40.0 + aside, 124.0),
        rect.max - vec2(40.0, 44.0),
    );
    let (world, size) = arrange(&painter, design, &project);
    let fit = (room.width() / size.x.max(1.0))
        .min(room.height() / size.y.max(1.0))
        .clamp(0.25, 1.35);

    // The wheel zooms about the pointer; dragging moves the plan.
    if response.dragged() {
        plan.pan += response.drag_delta();
    }
    if let Some(pointer) = response.hover_pos() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            let before = plan.zoom;
            plan.zoom = (plan.zoom * (scroll * 0.0018).exp()).clamp(0.4, 4.0);
            let from_centre = pointer - room.center() - plan.pan;
            plan.pan += from_centre - from_centre * (plan.zoom / before);
        }
    }
    let scale = fit * plan.zoom;
    let centre = room.center() + plan.pan;
    let to_screen = |r: Rect| {
        Rect::from_min_max(
            centre + r.min.to_vec2() * scale,
            centre + r.max.to_vec2() * scale,
        )
    };
    let cards: Vec<Rect> = world.iter().map(|r| to_screen(*r)).collect();

    plan.hovered = response
        .hover_pos()
        .and_then(|p| cards.iter().position(|c| c.contains(p)));
    // A double click opens a part onto its own parts: the one its first
    // click chose, as choosing slides the plan aside, from under the
    // pointer.
    let chosen_before = plan.chosen;
    let mut opening = None;
    if response.double_clicked()
        && let Some(c) = chosen_before.or(plan.hovered)
        && opens(&design.components[c])
    {
        opening = Some(design.components[c].path.clone());
    } else if response.clicked() {
        plan.chosen = plan.hovered;
        plan.chosen_at = now;
    }
    let settled = now - plan.chosen_at > DOUBLE_CLICK;
    if plan.chosen.is_some() && !settled {
        moving = true;
    }
    let shown_chosen = plan.chosen.filter(|_| settled);
    let focus = plan.hovered.or(plan.chosen);

    // The cards rise into place layer by layer as the plan opens.
    let since = now - plan.opened_at;
    let risen = |layer: usize| {
        let t = ((since - RISE_STAGGER * layer as f64) / RISE).clamp(0.0, 1.0) as f32;
        1.0 - (1.0 - t).powi(3)
    };
    if since < RISE + RISE_STAGGER * design.layers.len() as f64 {
        moving = true;
    }

    grid(&painter, rect, centre, scale);
    layer_bands(&painter, design, &cards, rect, scale);

    // Links under the cards. Each card's ends are spread along its edge in
    // the order of what they reach, so that they do not cross at the card.
    let layers = design.layers.len();
    let ports = |c: usize, outgoing: bool| -> Vec<usize> {
        let mut links: Vec<usize> = (0..design.links.len())
            .filter(|&l| {
                let link = design.links[l];
                if outgoing {
                    link.from == c && !link.cyclic
                } else {
                    link.to == c && !link.cyclic
                }
            })
            .collect();
        links.sort_by(|&a, &b| {
            let other = |l: usize| {
                let link = design.links[l];
                cards[if outgoing { link.to } else { link.from }].center().x
            };
            other(a).total_cmp(&other(b))
        });
        links
    };
    let port = |c: usize, l: usize, outgoing: bool| -> Pos2 {
        let ends = ports(c, outgoing);
        let i = ends.iter().position(|&e| e == l).unwrap_or(0);
        let card = cards[c];
        let x = card.left() + card.width() * (i as f32 + 1.0) / (ends.len() as f32 + 1.0);
        if outgoing {
            pos2(x, card.bottom())
        } else {
            pos2(x, card.top())
        }
    };
    let most = design
        .links
        .iter()
        .map(|l| l.imports)
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    // The counts of the links lit, drawn over the cards.
    let mut pills = Vec::new();
    for (l, link) in design.links.iter().enumerate() {
        let from_layer = design.components[link.from].layer;
        let rise = risen(from_layer).min(risen(design.components[link.to].layer));
        if rise <= 0.0 {
            continue;
        }
        let points = if link.cyclic {
            // Around a loop, in one layer: an arc above the row.
            let a = cards[link.from].center_top();
            let b = cards[link.to].center_top();
            let lift = vec2(0.0, -(40.0 + 0.2 * (b.x - a.x).abs()) * scale.min(1.0));
            let side = if link.from < link.to { 6.0 } else { -6.0 } * scale;
            let a = a + vec2(side, 0.0);
            let b = b + vec2(side, 0.0);
            [a, a + lift, b + lift, b]
        } else {
            let a = port(link.from, l, true);
            let b = port(link.to, l, false);
            let bend = vec2(0.0, (b.y - a.y) * 0.5);
            [a, a + bend, b - bend, b]
        };
        let lit = match focus {
            None => None,
            Some(f) if link.from == f => Some(ACCENT),
            Some(f) if link.to == f => Some(CYAN),
            Some(_) => Some(Color32::TRANSPARENT),
        };
        let base = if link.cyclic {
            RED
        } else {
            layer_colour(from_layer, layers)
        };
        let (colour, alpha) = match lit {
            // Loops stay red but quiet until pointed at: in a tangled level
            // they would hide all else.
            None => (base, if link.cyclic { 0.32 } else { 0.42 }),
            Some(c) if c == Color32::TRANSPARENT => (base, 0.07),
            Some(c) => (if link.cyclic { RED } else { c }, 0.95),
        };
        let alpha = alpha * rise;
        let width = (1.0 + 2.6 * (link.imports as f32).ln_1p() / most.ln_1p()) * scale.sqrt();
        // A wide faint stroke under a thin bright one: the link glows.
        for (w, a) in [(width * 5.0, 0.07), (width * 2.4, 0.16), (width, 1.0)] {
            painter.add(CubicBezierShape::from_points_stroke(
                points,
                false,
                Color32::TRANSPARENT,
                Stroke::new(w, faded(colour, alpha * a)),
            ));
        }
        arrow(&painter, points, faded(colour, alpha), 5.0 * scale.sqrt());
        if lit.is_some_and(|c| c != Color32::TRANSPARENT) {
            // In the gap beside the card at the far end, where the links
            // of the card pointed at have spread apart and no card is.
            let middle = if link.cyclic {
                bezier(points, 0.5)
            } else {
                let gap = ROW_GAP * scale / 2.0;
                let y = if focus == Some(link.from) {
                    points[3].y - gap
                } else {
                    points[0].y + gap
                };
                bezier(points, at_height(points, y))
            };
            let text = format!(
                "{} import{}{}",
                link.imports,
                if link.imports == 1 { "" } else { "s" },
                if link.cyclic { " · loop" } else { "" }
            );
            pills.push((middle, text, colour));
        }
    }

    // The cards.
    let biggest = design
        .components
        .iter()
        .map(|c| c.lines)
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    for (c, component) in design.components.iter().enumerate() {
        let rise = risen(component.layer);
        if rise <= 0.0 {
            continue;
        }
        let card = cards[c].translate(vec2(0.0, (1.0 - rise) * 26.0 * scale));
        let colour = layer_colour(component.layer, layers);
        let related = focus.is_none_or(|f| {
            f == c
                || design
                    .links
                    .iter()
                    .any(|l| (l.from == f && l.to == c) || (l.to == f && l.from == c))
        });
        let alpha = rise * if related { 1.0 } else { 0.32 };
        let picked = focus == Some(c);
        let radius = CornerRadius::same((10.0 * scale.min(1.4)) as u8);

        let shadow = Shadow {
            offset: [0, (6.0 * scale) as i8],
            blur: (22.0 * scale) as u8,
            spread: 0,
            color: Color32::from_black_alpha((150.0 * alpha) as u8),
        };
        painter.add(shadow.as_shape(card, radius));
        // A part that opens onto more: a stack, the cards under it showing.
        if opens(component) {
            for depth in [2.0, 1.0] {
                let under = card.translate(vec2(5.0, 5.0) * depth * scale.min(1.4));
                painter.rect(
                    under,
                    radius,
                    mix(theme::BACKGROUND, RAISED, alpha.max(0.6) * 0.8),
                    Stroke::new(1.0, faded(colour, 0.25 * alpha)),
                    StrokeKind::Inside,
                );
            }
        }
        if picked {
            // A halo around the card pointed at or chosen.
            for (grow, a) in [(10.0, 0.06), (5.0, 0.12), (2.0, 0.25)] {
                painter.rect_stroke(
                    card.expand(grow * scale),
                    radius,
                    Stroke::new(2.0 * scale, faded(colour, a)),
                    StrokeKind::Outside,
                );
            }
        }
        painter.rect(
            card,
            radius,
            // Opaque: the links that pass behind it stay behind it.
            mix(theme::BACKGROUND, mix(RAISED, colour, 0.07), alpha.max(0.6)),
            Stroke::new(
                if picked { 1.6 } else { 1.0 },
                faded(colour, if picked { 0.95 } else { 0.45 } * alpha),
            ),
            StrokeKind::Inside,
        );
        // A bar in the layer's colour along the top, the card's mark.
        painter.rect_filled(
            Rect::from_min_size(
                card.left_top() + vec2(14.0 * scale, 0.0),
                vec2(28.0 * scale, 3.0 * scale),
            ),
            CornerRadius::same(2),
            faded(colour, alpha),
        );
        painter.text(
            card.left_top() + vec2(16.0, 14.0) * scale,
            Align2::LEFT_TOP,
            title(component, &project),
            name_font(scale),
            faded(TEXT, alpha),
        );
        painter.text(
            card.left_top() + vec2(16.0, 37.0) * scale,
            Align2::LEFT_TOP,
            subtitle(component),
            small_font(scale),
            faded(DIM, alpha),
        );
        // How big it is next to the biggest, along the bottom.
        let track = Rect::from_min_size(
            card.left_bottom() + vec2(16.0, -11.0) * scale,
            vec2(card.width() - 32.0 * scale, 3.0 * scale),
        );
        painter.rect_filled(track, CornerRadius::same(2), faded(EDGE, alpha));
        let share = (component.lines as f32 / biggest).max(0.03);
        painter.rect_filled(
            Rect::from_min_size(track.min, vec2(track.width() * share, track.height())),
            CornerRadius::same(2),
            faded(colour, 0.85 * alpha),
        );

        if let Some(touch) = &plan.touches[c] {
            let shows = if touch.edited { EDIT_SHOWS } else { READ_SHOWS };
            let age = now - touch.at;
            if age < shows {
                moving = true;
                agent_was_here(&painter, card, radius, touch, (age / shows) as f32, scale);
            }
        }
    }

    for (at, text, colour) in pills {
        pill(&painter, at, &text, colour);
    }

    if let Some(folder) = header(ui, &painter, rect, app, design, &project) {
        opening = Some(folder);
    }
    painter.text(
        rect.left_bottom() + vec2(24.0, -18.0),
        Align2::LEFT_BOTTOM,
        "point at a part for its links · click it for its files · double-click to open it · drag to move · scroll to zoom · Ctrl-N: the universe",
        FontId::proportional(12.0),
        DIM,
    );

    if let Some(chosen) = shown_chosen {
        details(ui, rect, plan, chosen, app, &project);
    }
    if let Some(folder) = opening {
        plan.open(&folder, now);
    }
    if moving {
        ui.ctx().request_repaint();
    }
}

/// A faint grid of dots that moves with the plan: the paper it is drawn on.
fn grid(painter: &Painter, rect: Rect, centre: Pos2, scale: f32) {
    let step = 26.0 * scale.clamp(0.6, 2.0);
    let first = |from: f32, origin: f32| origin - ((origin - from) / step).floor() * step;
    let dot = faded(EDGE, 0.55);
    let mut y = first(rect.top(), centre.y);
    while y < rect.bottom() {
        let mut x = first(rect.left(), centre.x);
        while x < rect.right() {
            painter.circle_filled(pos2(x, y), 1.0, dot);
            x += step;
        }
        y += step;
    }
}

/// A faint band behind each layer, named at its top left.
fn layer_bands(painter: &Painter, design: &Architecture, cards: &[Rect], rect: Rect, scale: f32) {
    let layers = design.layers.len();
    for (layer, row) in design.layers.iter().enumerate() {
        // A layer that wraps spans all its rows.
        let Some(card) = row.iter().map(|&c| cards[c]).reduce(|a, b| a.union(b)) else {
            continue;
        };
        // Room above the cards for the layer's name.
        let band = Rect::from_min_max(
            pos2(rect.left(), card.top() - (22.0 * scale).max(17.0)),
            pos2(rect.right(), card.bottom() + 12.0 * scale),
        );
        painter.rect_filled(
            band,
            CornerRadius::ZERO,
            faded(layer_colour(layer, layers), 0.025),
        );
        let name = match layer {
            0 => "foundations".to_owned(),
            l if l + 1 == layers => format!("layer {l} · entry points"),
            l => format!("layer {l}"),
        };
        painter.text(
            pos2(rect.left() + 24.0, band.top() + 3.0),
            Align2::LEFT_TOP,
            name,
            FontId::proportional(11.0),
            faded(layer_colour(layer, layers), 0.55),
        );
    }
}

/// An arrowhead at the end of `points`, along the curve.
fn arrow(painter: &Painter, points: [Pos2; 4], colour: Color32, size: f32) {
    let tip = points[3];
    let back = bezier(points, 0.96);
    let along = (tip - back).normalized();
    if !along.x.is_finite() {
        return;
    }
    let across = vec2(-along.y, along.x);
    painter.add(Shape::convex_polygon(
        vec![
            tip,
            tip - along * size * 1.8 + across * size,
            tip - along * size * 1.8 - across * size,
        ],
        colour,
        Stroke::NONE,
    ));
}

/// A small label on a dark pill, for a link's count.
fn pill(painter: &Painter, at: Pos2, text: &str, colour: Color32) {
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::proportional(11.5), colour);
    let rect = Rect::from_center_size(at, galley.size() + vec2(14.0, 6.0));
    painter.rect(
        rect,
        CornerRadius::same(9),
        theme::see(theme::BACKGROUND, 0.92),
        Stroke::new(1.0, faded(colour, 0.6)),
        StrokeKind::Inside,
    );
    painter.galley(rect.center() - galley.size() / 2.0, galley, colour);
}

/// The agent's mark on a card: a ring spreading out and a glow fading, with
/// the file's name above; orange for an edit, blue for a read.
fn agent_was_here(
    painter: &Painter,
    card: Rect,
    radius: CornerRadius,
    touch: &Touch,
    age: f32,
    scale: f32,
) {
    let colour = if touch.edited {
        Color32::from_rgb(255, 150, 70)
    } else {
        Color32::from_rgb(110, 170, 255)
    };
    let fade = 1.0 - age;
    let rings = if touch.edited { 2 } else { 1 };
    for ring in 0..rings {
        let t = (age * 1.6 - ring as f32 * 0.25).clamp(0.0, 1.0);
        if t <= 0.0 || t >= 1.0 {
            continue;
        }
        painter.rect_stroke(
            card.expand(t * 26.0 * scale),
            radius,
            Stroke::new(2.0 * scale * (1.0 - t), faded(colour, 1.0 - t)),
            StrokeKind::Outside,
        );
    }
    for (grow, a) in [(8.0, 0.10), (4.0, 0.22), (1.5, 0.5)] {
        painter.rect_stroke(
            card.expand(grow * scale),
            radius,
            Stroke::new(2.0 * scale, faded(colour, a * fade)),
            StrokeKind::Outside,
        );
    }
    let verb = if touch.edited { "editing" } else { "reading" };
    let text = format!("{verb} {}", touch.file);
    let at = card.center_top() - vec2(0.0, 12.0 * scale);
    let galley = painter.layout_no_wrap(text, FontId::proportional(12.0), faded(colour, fade));
    let pill = Rect::from_center_size(at, galley.size() + vec2(14.0, 6.0));
    painter.rect(
        pill,
        CornerRadius::same(9),
        faded(theme::BACKGROUND, 0.9 * fade),
        Stroke::new(1.0, faded(colour, 0.7 * fade)),
        StrokeKind::Inside,
    );
    painter.galley(pill.center() - galley.size() / 2.0, galley, colour);
}

/// The title, what the plan holds, and the switch between the views.
/// Returns the folder of the way there clicked, to show its parts.
fn header(
    ui: &mut Ui,
    painter: &Painter,
    rect: Rect,
    app: &mut App,
    design: &Architecture,
    project: &str,
) -> Option<String> {
    let corner = rect.left_top() + vec2(24.0, 22.0);
    let title = painter.text(
        corner,
        Align2::LEFT_TOP,
        "Plan",
        FontId::proportional(26.0),
        TEXT,
    );
    let clicked = trail(
        ui,
        title.right_center() + vec2(16.0, 0.0),
        &design.level,
        project,
    );
    let parts = design.components.len();
    let links = design.links.len();
    let summary = format!(
        "{} · {parts} part{} · {links} link{}",
        if design.level.is_empty() {
            project
        } else {
            design.level.as_str()
        },
        if parts == 1 { "" } else { "s" },
        if links == 1 { "" } else { "s" },
    );
    let galley = painter.layout_no_wrap(summary, FontId::proportional(13.0), DIM);
    let width = galley.size().x;
    painter.galley(corner + vec2(0.0, 34.0), galley, DIM);
    let loops = design.cycles();
    let (said, colour) = if loops == 0 {
        ("  ·  no loop".to_owned(), theme::GREEN)
    } else {
        (
            format!(
                "  ·  {loops} link{} in a loop",
                if loops == 1 { "" } else { "s" }
            ),
            RED,
        )
    };
    painter.text(
        corner + vec2(width, 34.0),
        Align2::LEFT_TOP,
        said,
        FontId::proportional(13.0),
        colour,
    );
    let mut at = corner + vec2(0.0, 60.0);
    for (colour, what) in [(ACCENT, "uses"), (CYAN, "used by"), (RED, "in a loop")] {
        painter.line_segment(
            [at + vec2(0.0, 7.0), at + vec2(18.0, 7.0)],
            Stroke::new(2.5, colour),
        );
        let galley = painter.layout_no_wrap(what.to_owned(), FontId::proportional(12.0), DIM);
        let width = galley.size().x;
        painter.galley(at + vec2(24.0, 0.0), galley, DIM);
        at.x += 24.0 + width + 20.0;
    }
    switch(ui, rect, app);
    clicked
}

/// The way from the project to the level shown, each step a click away:
/// `ironquill › providers › apache`. Returns the folder clicked.
fn trail(ui: &mut Ui, at: Pos2, level: &str, project: &str) -> Option<String> {
    let mut steps = vec![(project.to_owned(), String::new())];
    let mut path = String::new();
    for part in level.split('/').filter(|p| !p.is_empty()) {
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(part);
        steps.push((part.to_owned(), path.clone()));
    }
    let mut clicked = None;
    let area = Rect::from_min_size(at - vec2(0.0, 13.0), vec2(900.0, 26.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(area)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let last = steps.len() - 1;
            for (i, (name, folder)) in steps.into_iter().enumerate() {
                if i > 0 {
                    ui.label(RichText::new("›").size(15.0).color(DIM));
                }
                if i == last {
                    ui.label(RichText::new(name).size(15.0).color(ACCENT));
                } else if ui
                    .add(egui::Button::new(RichText::new(name).size(15.0).color(TEXT)).frame(false))
                    .on_hover_text("show its parts")
                    .clicked()
                {
                    clicked = Some(folder);
                }
            }
        },
    );
    clicked
}

/// The two views of the codebase, top right, to pick with the mouse.
pub(crate) fn switch(ui: &mut Ui, rect: Rect, app: &mut App) {
    let area = Rect::from_min_size(rect.right_top() + vec2(-230.0, 20.0), vec2(206.0, 30.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(area)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
        |ui| {
            for (name, view) in [("Universe", MapView::Universe), ("Plan", MapView::Plan)] {
                let on = app.map_view() == Some(view);
                let text = RichText::new(name)
                    .size(14.0)
                    .color(if on { TEXT } else { DIM });
                if ui.add(egui::Button::selectable(on, text)).clicked() && !on {
                    app.show_map(Some(view));
                }
            }
        },
    );
}

/// The chosen component: what it holds, what it uses and what uses it,
/// each file a click away.
fn details(ui: &mut Ui, rect: Rect, plan: &mut Plan, chosen: usize, app: &mut App, project: &str) {
    let (Some(design), Some(map)) = (&plan.design, &plan.map) else {
        return;
    };
    let component = &design.components[chosen];
    let colour = layer_colour(component.layer, design.layers.len());
    let top = rect.left_top() + vec2(24.0, 140.0);
    let height = (rect.bottom() - 90.0 - top.y).max(120.0);
    let mut choose = None;
    let mut open = None;
    let mut close = false;
    let mut dive = None;
    egui::Area::new(Id::new("plan-details"))
        .fixed_pos(top)
        .show(ui.ctx(), |ui| {
            Frame::new()
                .fill(theme::see(PANEL, 0.94))
                .stroke(Stroke::new(1.0, faded(colour, 0.6)))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(16))
                .show(ui, |ui| {
                    ui.set_width(DETAILS_WIDTH - 32.0);
                    ui.set_max_height(height);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(title(component, project))
                                .size(20.0)
                                .color(TEXT),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("close").clicked() {
                                close = true;
                            }
                            if opens(component)
                                && ui
                                    .small_button("open")
                                    .on_hover_text("its own parts (double-click it)")
                                    .clicked()
                            {
                                dive = Some(component.path.clone());
                            }
                        });
                    });
                    let place = if component.path.is_empty() {
                        "the project's root".to_owned()
                    } else {
                        component.path.clone()
                    };
                    ui.label(RichText::new(place).monospace().size(12.0).color(DIM));
                    ui.label(
                        RichText::new(format!(
                            "{} · layer {}",
                            subtitle(component),
                            component.layer
                        ))
                        .size(12.5)
                        .color(colour),
                    );
                    ui.add_space(8.0);
                    ScrollArea::vertical().show(ui, |ui| {
                        let mut related =
                            |ui: &mut Ui, heading: &str, tint: Color32, uses: bool| {
                                let mut links: Vec<_> = design
                                    .links
                                    .iter()
                                    .filter(|l| {
                                        if uses {
                                            l.from == chosen
                                        } else {
                                            l.to == chosen
                                        }
                                    })
                                    .collect();
                                if links.is_empty() {
                                    return;
                                }
                                links.sort_by_key(|l| std::cmp::Reverse(l.imports));
                                ui.label(RichText::new(heading).size(12.0).color(tint));
                                for link in links {
                                    let other = if uses { link.to } else { link.from };
                                    let name = title(&design.components[other], project);
                                    let text = format!(
                                        "{name}  ·  {} import{}{}",
                                        link.imports,
                                        if link.imports == 1 { "" } else { "s" },
                                        if link.cyclic { " · loop" } else { "" }
                                    );
                                    let tint = if link.cyclic { RED } else { TEXT };
                                    if ui
                                        .add(
                                            egui::Button::new(RichText::new(text).color(tint))
                                                .frame(false),
                                        )
                                        .clicked()
                                    {
                                        choose = Some(other);
                                    }
                                }
                                ui.add_space(6.0);
                            };
                        related(ui, "USES", ACCENT, true);
                        related(ui, "USED BY", CYAN, false);
                        ui.label(RichText::new("FILES").size(12.0).color(DIM));
                        let longest =
                            component.files.first().map_or(1, |&f| lines(map, f)).max(1) as f32;
                        for &file in &component.files {
                            let node = &map.nodes[file];
                            let shown = node
                                .path
                                .strip_prefix(&component.path)
                                .map_or(node.path.as_str(), |p| p.trim_start_matches('/'));
                            let shown = if shown.is_empty() { node.name() } else { shown };
                            let row = ui.add(
                                egui::Button::new(
                                    RichText::new(format!("{shown}  {}", lines(map, file)))
                                        .monospace()
                                        .size(12.5)
                                        .color(TEXT),
                                )
                                .frame(false),
                            );
                            // How long it is, as a line under its name.
                            let share = lines(map, file) as f32 / longest;
                            let under = Rect::from_min_size(
                                row.rect.left_bottom() + vec2(4.0, -2.0),
                                vec2((DETAILS_WIDTH - 60.0) * share, 1.5),
                            );
                            ui.painter()
                                .rect_filled(under, CornerRadius::ZERO, faded(colour, 0.5));
                            if row.on_hover_text("open it").clicked() {
                                open = Some(node.path.clone());
                            }
                        }
                    });
                });
        });
    if close {
        plan.chosen = None;
    }
    if choose.is_some() {
        plan.chosen = choose;
    }
    if let Some(path) = open {
        app.open_path(plan.root.join(path));
        app.show_map(None);
    }
    if let Some(folder) = dive {
        let now = ui.input(|i| i.time);
        plan.open(&folder, now);
    }
}

fn lines(map: &CodeMap, node: usize) -> usize {
    match map.nodes[node].kind {
        ironquill_codemap::NodeKind::File { lines, .. } => lines,
        ironquill_codemap::NodeKind::Folder => 0,
    }
}

#[cfg(test)]
mod tests {
    use ironquill_tools::ToolSummary;

    use super::*;

    fn write(root: &std::path::Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn tool(path: &str, edited: bool) -> Entry {
        let outcome = if edited {
            ToolSummary::Changed {
                path: path.into(),
                created: false,
                diff: Vec::new(),
            }
        } else {
            ToolSummary::Read {
                path: path.into(),
                lines: 3,
            }
        };
        Entry::Tool {
            name: "tool".into(),
            path: Some(path.into()),
            outcome: Ok(outcome),
        }
    }

    #[test]
    fn opening_reads_the_project_and_only_later_work_lights_its_part() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a/x.py", "import b.y\n");
        write(dir.path(), "b/y.py", "");
        let mut plan = Plan::new(dir.path().to_owned());
        let history = vec![tool("a/x.py", true)];
        plan.showing(true, 1.0, &history);
        // Read on a thread of its own: taken once there.
        let start = std::time::Instant::now();
        while plan.is_reading() && start.elapsed().as_secs() < 10 {
            plan.take_read(1.0);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(plan.design.as_ref().map(|d| d.components.len()), Some(2));

        plan.light_up(&history, 1.0);
        assert!(
            plan.touches.iter().all(Option::is_none),
            "history stays dark"
        );

        let mut later = history.clone();
        later.push(tool("b/y.py", false));
        later.push(tool("a/x.py", true));
        later.push(tool("a/x.py", false));
        plan.light_up(&later, 2.0);
        let touch = |c: usize| {
            plan.touches[c]
                .as_ref()
                .map(|t| (t.edited, t.file.as_str()))
        };
        // A read right after an edit does not hide it.
        assert_eq!(touch(0), Some((true, "x.py")));
        assert_eq!(touch(1), Some((false, "y.py")));
    }

    #[test]
    fn a_part_opens_onto_its_own_parts_and_the_way_back_leads_to_the_whole() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/api/views.py", "import app.models.user\n");
        write(dir.path(), "app/models/user.py", "");
        write(dir.path(), "web/main.ts", "");
        let mut plan = Plan::new(dir.path().to_owned());
        plan.map = Some(ironquill_codemap::map(dir.path(), 100));
        plan.open("", 0.0);
        let names = |plan: &Plan| -> Vec<String> {
            let design = plan.design.as_ref().unwrap();
            design.components.iter().map(|c| c.name.clone()).collect()
        };
        assert_eq!(names(&plan), ["app", "web"]);
        plan.open("app", 1.0);
        assert_eq!(names(&plan), ["api", "models"]);
        assert_eq!(plan.design.as_ref().map(|d| d.links.len()), Some(1));
        // A folder gone: the whole again.
        plan.open("gone", 2.0);
        assert_eq!(names(&plan), ["app", "web"]);
    }

    #[test]
    fn layers_go_from_cool_foundations_to_warm_tops() {
        assert_eq!(layer_colour(0, 3), CYAN);
        assert_eq!(layer_colour(2, 3), ACCENT);
        assert_eq!(layer_colour(0, 1), CYAN);
        assert_eq!(thousands(980), "980");
        // However far the plan zooms, text takes a few sizes only.
        assert!((text_size(15.2) - 16.0).abs() < f32::EPSILON);
        assert!((text_size(3.0) - 7.0).abs() < f32::EPSILON);
        assert!((text_size(64.0) - 30.0).abs() < f32::EPSILON);
        assert_eq!(thousands(10_662), "10.7k");
        let down = [
            pos2(0.0, 0.0),
            pos2(0.0, 50.0),
            pos2(10.0, 50.0),
            pos2(10.0, 100.0),
        ];
        assert!((bezier(down, at_height(down, 80.0)).y - 80.0).abs() < 0.01);
    }
}
