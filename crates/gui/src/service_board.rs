//! A service laid out flat, a click away from its star: its objects as
//! cards in columns, the way a request goes (the way in, Services,
//! workloads, scaling and policies, configuration), arrows for how they
//! act on each other; on the right, its images beside the same service's
//! in the other environments and its values layer by layer, or everything
//! about the card chosen.

use bevy_egui::egui::epaint::CubicBezierShape;
use bevy_egui::egui::{
    self, Align2, Color32, CornerRadius, FontId, Frame, Id, Margin, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};
use universe_codemap::{Acts, Fleet, Interaction, Piece, Place, Star, interactions};
use universe_ui::App;

use crate::fleet_view::{egui_colour, image_colour, kind_colour, layer_colour};
use crate::infra::Infra;
use crate::theme::{self, CODE, DIM, EDGE, LINK, PANEL, RAISED, TEXT, YELLOW};

/// A card's height, and the room between cards and between columns.
const CARD_HEIGHT: f32 = 64.0;
const GAP: f32 = 16.0;
const COLUMN_GAP: f32 = 40.0;

/// Where an object stands, the way a request goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Column {
    WayIn,
    Services,
    Workloads,
    Scaling,
    Configuration,
    Other,
}

impl Column {
    const ALL: [Self; 6] = [
        Self::WayIn,
        Self::Services,
        Self::Workloads,
        Self::Scaling,
        Self::Configuration,
        Self::Other,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::WayIn => "Way in",
            Self::Services => "Services",
            Self::Workloads => "Workloads",
            Self::Scaling => "Scaling and policies",
            Self::Configuration => "Configuration",
            Self::Other => "Other",
        }
    }
}

/// The column of an object of `kind`.
pub(crate) fn column(kind: &str) -> Column {
    match kind {
        "Ingress" | "IngressRoute" | "HTTPRoute" | "GRPCRoute" | "TCPRoute" | "TLSRoute"
        | "Gateway" | "VirtualService" => Column::WayIn,
        "Service" => Column::Services,
        "Deployment" | "StatefulSet" | "DaemonSet" | "ReplicaSet" | "Job" | "CronJob" | "Pod"
        | "Rollout" => Column::Workloads,
        "HorizontalPodAutoscaler"
        | "VerticalPodAutoscaler"
        | "PodDisruptionBudget"
        | "NetworkPolicy"
        | "ScaledObject" => Column::Scaling,
        "ConfigMap"
        | "Secret"
        | "SealedSecret"
        | "ExternalSecret"
        | "ServiceAccount"
        | "PersistentVolumeClaim"
        | "Role"
        | "RoleBinding"
        | "ClusterRole"
        | "ClusterRoleBinding" => Column::Configuration,
        _ => Column::Other,
    }
}

/// The colour of a way of acting.
fn acts_colour(how: Acts) -> Color32 {
    match how {
        Acts::Routes => YELLOW,
        Acts::Selects => LINK,
        Acts::Scales => theme::CYAN,
        Acts::Protects => Color32::from_rgb(120, 210, 180),
        Acts::Reads => theme::GREEN,
        Acts::RunsAs => theme::MAGENTA,
        Acts::Mounts => Color32::from_rgb(200, 190, 120),
    }
}

/// The service laid out, and the card chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Board {
    galaxy: usize,
    star: usize,
    /// The card chosen, by its place among the service's objects.
    chosen: Option<usize>,
    /// What the values are filtered by.
    filter: String,
}

impl Board {
    /// The board of star `star` of galaxy `galaxy`.
    pub(crate) fn new(galaxy: usize, star: usize) -> Self {
        Self {
            galaxy,
            star,
            chosen: None,
            filter: String::new(),
        }
    }

    /// Finds the same service in `new`, by its environment's name and its
    /// own, after `old` was assembled again; false when it is gone.
    pub(crate) fn find_again(&mut self, old: Option<&Fleet>, new: &Fleet) -> bool {
        let Some((environment, service)) = old.and_then(|old| {
            let galaxy = old.galaxies.get(self.galaxy)?;
            Some((
                galaxy.name.as_str(),
                galaxy.stars.get(self.star)?.name.as_str(),
            ))
        }) else {
            return false;
        };
        let found = new
            .galaxies
            .iter()
            .enumerate()
            .find(|(_, g)| g.name == environment)
            .and_then(|(g, galaxy)| {
                Some((g, galaxy.stars.iter().position(|s| s.name == service)?))
            });
        match found {
            Some((galaxy, star)) => {
                if (galaxy, star) != (self.galaxy, self.star) {
                    self.chosen = None;
                }
                self.galaxy = galaxy;
                self.star = star;
                true
            }
            None => false,
        }
    }
}

/// Lays the service out over `rect`. Returns whether to go back to the
/// universe.
pub(crate) fn show(
    ui: &mut Ui,
    rect: Rect,
    fleet: &Fleet,
    board: &mut Board,
    infra: &Infra,
    app: &mut App,
) -> bool {
    let Some(star) = fleet
        .galaxies
        .get(board.galaxy)
        .and_then(|g| g.stars.get(board.star))
    else {
        return true;
    };
    if board.chosen.is_some_and(|c| c >= star.pieces.len()) {
        board.chosen = None;
    }
    let mut back = false;
    let mut open: Option<Place> = None;
    let top = header(ui, rect, fleet, board, star, &mut back, &mut open);
    // A third of the room, so that the columns keep the rest.
    let width = (rect.width() * 0.34).clamp(290.0, 440.0);
    let details = Rect::from_min_max(
        pos2(rect.right() - width - 24.0, top),
        pos2(rect.right() - 24.0, rect.bottom() - 24.0),
    );
    let room = Rect::from_min_max(
        pos2(rect.left() + 24.0, top),
        pos2(details.left() - 24.0, rect.bottom() - 24.0),
    );
    let refs: Vec<&serde_json::Value> = star.pieces.iter().map(|p| &p.body).collect();
    let acting = interactions(&refs);
    if star.pieces.is_empty() {
        let galaxy = &fleet.galaxies[board.galaxy];
        let text = if galaxy.rendered {
            "Nothing rendered for this service: see what could not be rendered in the services view's notes"
                .to_owned()
        } else if infra.rendering() == Some(galaxy.name.as_str()) {
            format!("Rendering {} with kustomize and helm…", galaxy.name)
        } else {
            format!("{} is not rendered yet", galaxy.name)
        };
        ui.painter_at(room).text(
            room.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(15.0),
            DIM,
        );
    } else {
        cards(ui, room, star, &acting, &mut board.chosen);
    }
    ui.scope_builder(egui::UiBuilder::new().max_rect(details), |ui| {
        Frame::new()
            .fill(theme::see(PANEL, 0.96))
            .stroke(Stroke::new(1.0, EDGE))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(details.width() - 30.0);
                ui.set_min_height(details.height() - 30.0);
                ScrollArea::vertical()
                    .id_salt(("board-details", board.galaxy, board.star, board.chosen))
                    .auto_shrink([false, false])
                    .show(ui, |ui| match board.chosen {
                        Some(chosen) => {
                            object(ui, star, chosen, &acting, &mut board.chosen, &mut open);
                        }
                        None => service(ui, fleet, board, star, &mut open),
                    });
            });
    });
    if let Some(place) = open {
        crate::services_view::open_at(infra, &place, app);
    }
    back
}

/// A place, as a link that opens it.
fn place_link(ui: &mut Ui, place: &Place, text: &str, open: &mut Option<Place>) {
    let label = ui
        .add(egui::Label::new(RichText::new(text).size(12.5).color(LINK)).sense(Sense::click()))
        .on_hover_text(format!("Open {place}"))
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if label.clicked() {
        *open = Some(place.clone());
    }
}

/// The header: the way back, the service's name and namespace, a tab for
/// each environment that runs it, its chart and where it is declared.
/// Returns where the room below starts.
fn header(
    ui: &mut Ui,
    rect: Rect,
    fleet: &Fleet,
    board: &mut Board,
    star: &Star,
    back: &mut bool,
    open: &mut Option<Place>,
) -> f32 {
    let corner = rect.left_top() + vec2(24.0, 20.0);
    let mut switch = None;
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_size(
                corner,
                vec2((rect.width() - 560.0).max(320.0), 110.0),
            ))
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.horizontal(|ui| {
                let button = egui::Button::new(RichText::new("‹ Universe").size(13.0).color(TEXT))
                    .fill(theme::see(RAISED, 0.9))
                    .stroke(Stroke::new(1.0, EDGE));
                if ui
                    .add(button)
                    .on_hover_text("Back to the environments")
                    .clicked()
                {
                    *back = true;
                }
                ui.add_space(8.0);
                ui.label(RichText::new(&star.name).size(24.0).color(TEXT));
                if let Some(ns) = &star.namespace {
                    ui.label(RichText::new(format!("in {ns}")).size(14.0).color(DIM));
                }
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for (g, galaxy) in fleet.galaxies.iter().enumerate() {
                    let Some(s) = galaxy.stars.iter().position(|s| s.name == star.name) else {
                        continue;
                    };
                    let on = g == board.galaxy;
                    let colour = egui_colour(crate::fleet_view::galaxy_colour(g));
                    let text = RichText::new(&galaxy.name).size(13.0).color(if on {
                        colour
                    } else {
                        colour.gamma_multiply(0.6)
                    });
                    if ui
                        .add(egui::Button::selectable(on, text))
                        .on_hover_text(format!("{} in {}", star.name, galaxy.name))
                        .clicked()
                        && !on
                    {
                        switch = Some((g, s));
                    }
                }
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if let Some(chart) = &star.chart {
                    ui.label(
                        RichText::new(format!("chart {chart} ·"))
                            .size(12.5)
                            .color(DIM),
                    );
                }
                ui.label(RichText::new("declared in").size(12.5).color(DIM));
                place_link(ui, &star.place, &star.place.to_string(), open);
            });
        },
    );
    if let Some((galaxy, star)) = switch {
        board.galaxy = galaxy;
        board.star = star;
        board.chosen = None;
    }
    corner.y + 118.0
}

/// The cards in their columns, and the arrows between them.
fn cards(ui: &mut Ui, room: Rect, star: &Star, acting: &[Interaction], chosen: &mut Option<usize>) {
    let columns: Vec<Column> = Column::ALL
        .into_iter()
        .filter(|c| star.pieces.iter().any(|p| column(&p.kind) == *c))
        .collect();
    let count = columns.len().max(1) as f32;
    let width = ((room.width() - COLUMN_GAP * (count - 1.0)) / count).clamp(132.0, 230.0);
    let tallest = columns
        .iter()
        .map(|c| star.pieces.iter().filter(|p| column(&p.kind) == *c).count())
        .max()
        .unwrap_or(0) as f32;
    let content = vec2(
        count * width + (count - 1.0) * COLUMN_GAP,
        34.0 + tallest * (CARD_HEIGHT + GAP),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(room), |ui| {
        ScrollArea::both()
            .id_salt("board-cards")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (area, background) = ui.allocate_exact_size(
                    content.max(room.size() - vec2(16.0, 16.0)),
                    Sense::click(),
                );
                if background.clicked() {
                    *chosen = None;
                }
                let painter = ui.painter_at(area.intersect(ui.clip_rect()));
                let mut places: Vec<Rect> = vec![Rect::NOTHING; star.pieces.len()];
                for (c, col) in columns.iter().enumerate() {
                    let x = area.left() + c as f32 * (width + COLUMN_GAP);
                    painter.text(
                        pos2(x, area.top()),
                        Align2::LEFT_TOP,
                        col.title(),
                        FontId::proportional(12.5),
                        DIM,
                    );
                    let mut y = area.top() + 30.0;
                    for (i, _) in star
                        .pieces
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| column(&p.kind) == *col)
                    {
                        places[i] = Rect::from_min_size(pos2(x, y), vec2(width, CARD_HEIGHT));
                        y += CARD_HEIGHT + GAP;
                    }
                }
                arrows(&painter, acting, &places, *chosen);
                for (i, piece) in star.pieces.iter().enumerate() {
                    let card = places[i];
                    let response = ui
                        .interact(card, Id::new(("board-card", i)), Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if response.clicked() {
                        *chosen = if *chosen == Some(i) { None } else { Some(i) };
                    }
                    let near = chosen.is_none_or(|c| {
                        c == i
                            || acting
                                .iter()
                                .any(|a| (a.from, a.to) == (c, i) || (a.to, a.from) == (c, i))
                    });
                    draw_card(
                        &painter,
                        card,
                        piece,
                        star.namespace.as_deref(),
                        *chosen == Some(i),
                        response.hovered(),
                        near,
                    );
                }
            });
    });
}

/// A point of a cubic curve.
fn bezier(p: [Pos2; 4], t: f32) -> Pos2 {
    let u = 1.0 - t;
    let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    pos2(
        w.iter().zip(&p).map(|(w, p)| w * p.x).sum(),
        w.iter().zip(&p).map(|(w, p)| w * p.y).sum(),
    )
}

/// The arrows, coloured by how; those of the card chosen lit and named,
/// the rest dimmed.
fn arrows(painter: &egui::Painter, acting: &[Interaction], places: &[Rect], chosen: Option<usize>) {
    for interaction in acting {
        let (Some(a), Some(b)) = (places.get(interaction.from), places.get(interaction.to)) else {
            continue;
        };
        let lit = chosen.is_some_and(|c| c == interaction.from || c == interaction.to);
        let colour = acts_colour(interaction.how);
        let colour = match chosen {
            None => colour.gamma_multiply(0.75),
            Some(_) if lit => colour,
            Some(_) => colour.gamma_multiply(0.12),
        };
        let (start, end, bend) = if (a.center().x - b.center().x).abs() < 1.0 {
            // In one column: around its right side.
            (a.right_center(), b.right_center(), vec2(46.0, 0.0))
        } else if a.center().x < b.center().x {
            (a.right_center(), b.left_center(), vec2(0.0, 0.0))
        } else {
            (a.left_center(), b.right_center(), vec2(0.0, 0.0))
        };
        let reach = (end.x - start.x).abs().max(40.0) * 0.45;
        let towards = if end.x >= start.x { 1.0 } else { -1.0 };
        let points = if bend.x > 0.0 {
            [start, start + bend, end + bend, end]
        } else {
            [
                start,
                start + vec2(reach * towards, 0.0),
                end - vec2(reach * towards, 0.0),
                end,
            ]
        };
        painter.add(CubicBezierShape::from_points_stroke(
            points,
            false,
            Color32::TRANSPARENT,
            Stroke::new(if lit { 2.2 } else { 1.4 }, colour),
        ));
        // The head, along the curve's last stretch.
        let direction = (end - bezier(points, 0.92)).normalized();
        let side = vec2(-direction.y, direction.x);
        painter.add(egui::Shape::convex_polygon(
            vec![
                end,
                end - direction * 9.0 + side * 4.5,
                end - direction * 9.0 - side * 4.5,
            ],
            colour,
            Stroke::NONE,
        ));
        if lit {
            let middle = bezier(points, 0.5);
            let font = FontId::proportional(11.5);
            let galley = painter.layout_no_wrap(interaction.how.label().to_owned(), font, colour);
            let area = Rect::from_center_size(middle, galley.size() + vec2(10.0, 4.0));
            painter.rect_filled(area, 4.0, theme::BACKGROUND);
            painter.galley(area.min + vec2(5.0, 2.0), galley, colour);
        }
    }
}

/// A card: its kind in colour, its name, its main fact, and its namespace
/// when another than the service's.
fn draw_card(
    painter: &egui::Painter,
    card: Rect,
    piece: &Piece,
    namespace: Option<&str>,
    chosen: bool,
    hovered: bool,
    near: bool,
) {
    let kind = egui_colour(kind_colour(&piece.kind));
    let fade = if near { 1.0 } else { 0.35 };
    painter.rect(
        card,
        CornerRadius::same(8),
        if hovered || chosen {
            theme::SELECTED
        } else {
            RAISED
        },
        Stroke::new(
            if chosen { 1.6 } else { 1.0 },
            if chosen { kind } else { EDGE },
        ),
        StrokeKind::Inside,
    );
    painter.rect_filled(
        Rect::from_min_size(card.min, vec2(4.0, card.height())),
        CornerRadius {
            nw: 8,
            sw: 8,
            ne: 0,
            se: 0,
        },
        kind.gamma_multiply(fade),
    );
    let clip = painter.with_clip_rect(card.shrink(2.0));
    clip.text(
        card.min + vec2(12.0, 8.0),
        Align2::LEFT_TOP,
        &piece.kind,
        FontId::proportional(11.0),
        kind.gamma_multiply(fade),
    );
    if piece.namespace.as_deref() != namespace
        && let Some(ns) = &piece.namespace
    {
        clip.text(
            card.right_top() + vec2(-8.0, 8.0),
            Align2::RIGHT_TOP,
            ns,
            FontId::proportional(10.5),
            DIM.gamma_multiply(fade),
        );
    }
    clip.text(
        card.min + vec2(12.0, 23.0),
        Align2::LEFT_TOP,
        &piece.name,
        FontId::proportional(14.0),
        TEXT.gamma_multiply(fade),
    );
    if let Some(fact) = piece.facts.first() {
        clip.text(
            card.min + vec2(12.0, 43.0),
            Align2::LEFT_TOP,
            fact,
            FontId::proportional(11.5),
            DIM.gamma_multiply(fade),
        );
    }
}

/// The section title of the details.
fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text).size(15.0).color(TEXT));
    ui.add_space(2.0);
}

/// The service, with no card chosen: its images beside the same service's
/// elsewhere, and its values, each with the layers that set it.
fn service(ui: &mut Ui, fleet: &Fleet, board: &mut Board, star: &Star, open: &mut Option<Place>) {
    heading(ui, "Images");
    if star.images.is_empty() {
        ui.label(RichText::new("none found").size(12.5).color(DIM));
    }
    for image in &star.images {
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter()
                .circle_filled(dot.center(), 4.0, egui_colour(image_colour(image)));
            ui.label(RichText::new(image).size(12.5).color(CODE).monospace());
        });
    }
    let elsewhere: Vec<(&str, &Star)> = fleet
        .galaxies
        .iter()
        .enumerate()
        .filter(|(g, _)| *g != board.galaxy)
        .filter_map(|(_, galaxy)| {
            Some((
                galaxy.name.as_str(),
                galaxy.stars.iter().find(|s| s.name == star.name)?,
            ))
        })
        .collect();
    if !elsewhere.is_empty() {
        ui.add_space(4.0);
        for (environment, other) in elsewhere {
            let same = other.images == star.images;
            let text = if other.images.is_empty() {
                format!("{environment}: no image found")
            } else if same {
                format!("{environment}: the same")
            } else {
                format!("{environment}: {}", other.images.join(", "))
            };
            ui.label(
                RichText::new(text)
                    .size(12.0)
                    .color(if same || other.images.is_empty() {
                        DIM
                    } else {
                        YELLOW
                    }),
            );
        }
    }

    heading(ui, &format!("Values · {}", star.settings.len()));
    let won: Vec<String> = star
        .won_by()
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(kind, n)| format!("{n} by the {}", kind.label()))
        .collect();
    if !won.is_empty() {
        ui.label(
            RichText::new(format!("set last: {}", won.join(", ")))
                .size(12.0)
                .color(DIM),
        );
    }
    ui.add(
        egui::TextEdit::singleline(&mut board.filter)
            .hint_text("filter the values")
            .desired_width(f32::INFINITY),
    );
    ui.add_space(4.0);
    let filter = board.filter.to_lowercase();
    for setting in star.settings.iter().filter(|s| {
        filter.is_empty()
            || s.key.to_lowercase().contains(&filter)
            || s.value().to_lowercase().contains(&filter)
    }) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(
                RichText::new(&setting.key)
                    .size(12.5)
                    .color(CODE)
                    .monospace(),
            );
            ui.label(RichText::new("=").size(12.5).color(DIM));
            ui.label(
                RichText::new(setting.value())
                    .size(12.5)
                    .color(TEXT)
                    .monospace(),
            );
        });
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.add_space(12.0);
            for (k, layer) in setting.layers.iter().enumerate() {
                if k > 0 {
                    ui.label(RichText::new("→").size(11.0).color(DIM));
                }
                let colour = egui_colour(layer_colour(layer.kind));
                let chip = ui
                    .add(
                        egui::Label::new(
                            RichText::new(layer.kind.label()).size(11.5).color(colour),
                        )
                        .sense(Sense::click()),
                    )
                    .on_hover_text(format!("{} at {}", layer.value, layer.place))
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if chip.clicked() {
                    *open = Some(layer.place.clone());
                }
            }
        });
        ui.add_space(3.0);
    }
}

/// Everything about the object chosen: its facts, what it acts on and
/// what acts on it, its template, its patches, and the object as rendered.
fn object(
    ui: &mut Ui,
    star: &Star,
    chosen: usize,
    acting: &[Interaction],
    choose: &mut Option<usize>,
    open: &mut Option<Place>,
) {
    let piece = &star.pieces[chosen];
    if ui
        .add(
            egui::Label::new(
                RichText::new(format!("‹ {}", star.name))
                    .size(12.5)
                    .color(LINK),
            )
            .sense(Sense::click()),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        *choose = None;
    }
    ui.label(
        RichText::new(&piece.kind)
            .size(13.0)
            .color(egui_colour(kind_colour(&piece.kind))),
    );
    ui.label(RichText::new(&piece.name).size(18.0).color(TEXT));
    if let Some(ns) = &piece.namespace {
        ui.label(RichText::new(format!("in {ns}")).size(12.0).color(DIM));
    }
    if !piece.facts.is_empty() {
        heading(ui, "Facts");
        for fact in &piece.facts {
            ui.label(RichText::new(fact).size(12.5).color(TEXT));
        }
    }
    let mut related = |ui: &mut Ui, title: &str, pairs: Vec<(Acts, usize)>, outwards: bool| {
        if pairs.is_empty() {
            return;
        }
        heading(ui, title);
        for (how, other) in pairs {
            let target = &star.pieces[other];
            let text = if outwards {
                format!("{} {} {}", how.label(), target.kind, target.name)
            } else {
                format!("{} {} {}", target.kind, target.name, how.label())
            };
            if ui
                .add(
                    egui::Label::new(RichText::new(text).size(12.5).color(acts_colour(how)))
                        .sense(Sense::click()),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                *choose = Some(other);
            }
        }
    };
    let acts_on: Vec<(Acts, usize)> = acting
        .iter()
        .filter(|a| a.from == chosen)
        .map(|a| (a.how, a.to))
        .collect();
    let acted_on: Vec<(Acts, usize)> = acting
        .iter()
        .filter(|a| a.to == chosen)
        .map(|a| (a.how, a.from))
        .collect();
    related(ui, "Acts on", acts_on, true);
    related(ui, "Acted on by", acted_on, false);
    if let Some(template) = &piece.template {
        heading(ui, "Template");
        place_link(ui, template, &template.to_string(), open);
    }
    if !piece.patches.is_empty() {
        heading(ui, "Patched by");
        for patch in &piece.patches {
            place_link(ui, patch, &patch.to_string(), open);
        }
    }
    heading(ui, "As rendered");
    let mut text = String::new();
    yaml(&piece.body, 0, &mut text);
    ui.add(
        egui::Label::new(RichText::new(text).size(12.0).color(TEXT).monospace()).selectable(true),
    );
}

/// A scalar as YAML reads it; a string quoted when YAML could read it as
/// something else.
fn scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => {
            let plain = !s.is_empty()
                && !s.contains(": ")
                && !s.contains(" #")
                && !s.contains('\n')
                && !s.starts_with(|c: char| "-?:,[]{}#&*!|>'\"%@` ".contains(c))
                && !s.ends_with(' ')
                && s.parse::<f64>().is_err()
                && !["true", "false", "null", "yes", "no", "on", "off", "~"]
                    .contains(&s.to_lowercase().as_str());
            if plain {
                s.clone()
            } else {
                serde_json::Value::String(s.clone()).to_string()
            }
        }
        serde_json::Value::Object(m) if m.is_empty() => "{}".to_owned(),
        serde_json::Value::Array(a) if a.is_empty() => "[]".to_owned(),
        other => other.to_string(),
    }
}

/// Whether a value is written on its key's line.
fn inline(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(m) => m.is_empty(),
        serde_json::Value::Array(a) => a.is_empty(),
        _ => true,
    }
}

/// `value` as YAML, indented by `indent`.
fn yaml(value: &serde_json::Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match value {
        serde_json::Value::Object(map) if !map.is_empty() => {
            for (key, v) in map {
                if inline(v) {
                    out.push_str(&format!("{pad}{key}: {}\n", scalar(v)));
                } else {
                    out.push_str(&format!("{pad}{key}:\n"));
                    yaml(v, indent + 2, out);
                }
            }
        }
        serde_json::Value::Array(items) if !items.is_empty() => {
            for item in items {
                if inline(item) {
                    out.push_str(&format!("{pad}- {}\n", scalar(item)));
                } else {
                    // The item written two deeper, its first line's room
                    // taken by the dash.
                    let mut inner = String::new();
                    yaml(item, indent + 2, &mut inner);
                    out.push_str(&pad);
                    out.push_str("- ");
                    out.push_str(&inner[indent + 2..]);
                }
            }
        }
        other => out.push_str(&format!("{pad}{}\n", scalar(other))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objects_fall_into_the_way_a_request_goes() {
        let kinds = [
            ("Ingress", Column::WayIn),
            ("HTTPRoute", Column::WayIn),
            ("Service", Column::Services),
            ("Deployment", Column::Workloads),
            ("CronJob", Column::Workloads),
            ("HorizontalPodAutoscaler", Column::Scaling),
            ("PodDisruptionBudget", Column::Scaling),
            ("NetworkPolicy", Column::Scaling),
            ("ConfigMap", Column::Configuration),
            ("Secret", Column::Configuration),
            ("ServiceAccount", Column::Configuration),
            ("PersistentVolumeClaim", Column::Configuration),
            ("RoleBinding", Column::Configuration),
            ("ServiceMonitor", Column::Other),
        ];
        for (kind, expected) in kinds {
            assert_eq!(column(kind), expected, "{kind}");
        }
        // The columns read left to right in that order.
        assert!(Column::WayIn < Column::Services && Column::Services < Column::Workloads);
    }

    #[test]
    fn an_object_reads_as_yaml() {
        let body = serde_json::json!({
            "kind": "Deployment",
            "metadata": {"name": "api", "labels": {}},
            "spec": {"replicas": 3, "template": {"spec": {"containers": [
                {"name": "api", "image": "registry.example.com/api:1.2.0",
                 "args": ["--port", "8000"], "env": [{"name": "LOG", "value": "true"}]}
            ]}}}
        });
        let mut text = String::new();
        yaml(&body, 0, &mut text);
        assert_eq!(
            text,
            "kind: Deployment\nmetadata:\n  labels: {}\n  name: api\nspec:\n  replicas: 3\n  template:\n    spec:\n      containers:\n        - args:\n            - \"--port\"\n            - \"8000\"\n          env:\n            - name: LOG\n              value: \"true\"\n          image: registry.example.com/api:1.2.0\n          name: api\n"
        );
    }

    #[test]
    fn the_same_service_is_found_again_when_the_fleet_is_assembled_anew() {
        use universe_codemap::{Galaxy, Place};
        let star = |name: &str| Star {
            name: name.into(),
            namespace: None,
            chart: None,
            images: Vec::new(),
            pieces: Vec::new(),
            settings: Vec::new(),
            place: Place::new("ops", "a.yaml", 1),
        };
        let galaxy = |name: &str, stars: Vec<Star>| Galaxy {
            name: name.into(),
            named: true,
            rendered: false,
            stars,
        };
        let old = Fleet {
            galaxies: vec![galaxy("prod", vec![star("api"), star("web")])],
        };
        let new = Fleet {
            galaxies: vec![
                galaxy("dev", vec![star("web")]),
                galaxy("prod", vec![star("web"), star("api")]),
            ],
        };
        let mut board = Board::new(0, 1);
        assert!(board.find_again(Some(&old), &new));
        assert_eq!((board.galaxy, board.star), (1, 0));
        let gone = Fleet {
            galaxies: vec![galaxy("prod", vec![star("api")])],
        };
        assert!(!board.find_again(Some(&new), &gone));
    }
}
