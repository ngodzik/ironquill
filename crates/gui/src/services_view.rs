//! The codebase's services (Ctrl-N, after the plan): one in the middle,
//! those that reach it on the left, those it reaches on the right, a curve
//! for each way, blue through an OpenAPI spec, violet over MCP, yellow by
//! plain HTTP. A service shows what it serves and who reaches it; a link,
//! the routes called or the tools offered, and every place it was read.
//!
//! Drawn from the plan's map, which reads the services with the rest:
//! flat, painted by egui alone, nothing to wait for.

use bevy_egui::egui::epaint::CubicBezierShape;
use bevy_egui::egui::{
    self, Align2, Color32, CornerRadius, FontId, Frame, Id, Margin, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};
use ironquill_codemap::{CodeMap, Place, Services, ToolKind, Via};
use ironquill_ui::{App, MapView};

use crate::api_view::{self, ApiView};
use crate::infra::Infra;
use crate::plan::Plan;
use crate::theme::{
    self, ACCENT, CODE, CYAN, DIM, EDGE, LINK, MAGENTA, PANEL, RAISED, TEXT, YELLOW,
};

/// A card's size, and the room between cards of a column.
const CARD: egui::Vec2 = egui::Vec2::new(220.0, 64.0);
const GAP: f32 = 18.0;

/// The details' width, on the right.
const DETAILS_WIDTH: f32 = 380.0;

/// The width of a setting's value in the configuration.
const VALUE_WIDTH: f32 = 280.0;

/// What is chosen: a service, or a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chosen {
    Service(usize),
    Link(usize),
}

/// What the services view shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Who reaches whom.
    #[default]
    Links,
    /// The environment's architecture, flat.
    Architecture,
    /// A service's settings, layer by layer.
    Configuration,
}

/// What the services view remembers between frames.
#[derive(Default)]
pub(crate) struct ServicesView {
    /// The service put in the middle, when not the map's centre.
    centre: Option<usize>,
    chosen: Option<Chosen>,
    pub(crate) mode: Mode,
    /// What the configuration is filtered by.
    filter: String,
    /// The release whose configuration shows, by its label.
    release: Option<String>,
    /// The settings whose earlier layers show, by key.
    expanded: std::collections::HashSet<String>,
    /// The element of the architecture chosen.
    pub(crate) element: Option<usize>,
    /// Whether the notes show.
    notes: bool,
    /// The 3D architecture was asked for: the universe window takes it.
    pub(crate) open_3d: bool,
}

/// A way's colour and name.
fn look(via: Via) -> (Color32, &'static str) {
    match via {
        Via::OpenApi => (LINK, "OpenAPI"),
        Via::Mcp => (MAGENTA, "MCP"),
        Via::Http => (YELLOW, "HTTP"),
        Via::Cloud => (CYAN, "cloud"),
    }
}

/// What a link's label says: what it carries.
fn label(services: &Services, link: usize) -> String {
    let l = &services.links[link];
    match l.via {
        Via::OpenApi => format!(
            "OpenAPI · {} route{}",
            l.operations.len(),
            if l.operations.len() == 1 { "" } else { "s" }
        ),
        Via::Mcp => {
            let tools = services.services[l.to]
                .tools
                .iter()
                .filter(|t| t.kind == ToolKind::Tool)
                .count();
            if tools == 0 {
                "MCP".to_owned()
            } else {
                format!("MCP · {tools} tool{}", if tools == 1 { "" } else { "s" })
            }
        }
        Via::Http => "HTTP".to_owned(),
        Via::Cloud => services.services[l.to]
            .cloud
            .map_or("cloud", |k| k.label())
            .to_owned(),
    }
}

/// What a card says under a service's name.
fn subtitle(services: &Services, service: usize) -> String {
    let s = &services.services[service];
    if let Some(kind) = s.cloud {
        return format!("{} in the cloud", kind.label().to_lowercase());
    }
    if s.external {
        return "outside the project".to_owned();
    }
    if s.folder.is_none() && s.operations.is_empty() && s.tools.is_empty() {
        return "deployed".to_owned();
    }
    let tools = s.tools.iter().filter(|t| t.kind == ToolKind::Tool).count();
    let mut parts = Vec::new();
    if !s.operations.is_empty() {
        parts.push(format!(
            "{} route{}",
            s.operations.len(),
            if s.operations.len() == 1 { "" } else { "s" }
        ));
    }
    if tools > 0 {
        parts.push(format!(
            "{tools} MCP tool{}",
            if tools == 1 { "" } else { "s" }
        ));
    }
    if parts.is_empty() {
        s.folder
            .clone()
            .map_or_else(|| "an image".to_owned(), |f| format!("{f}/"))
    } else {
        parts.join(" · ")
    }
}

/// Where each service stands: the centre in the middle of `room`, those
/// it reaches on the right, those that only reach it on the left; the
/// others, not linked to it, left out.
fn arrange(services: &Services, centre: usize, room: Rect) -> Vec<Option<Rect>> {
    let mut left: Vec<usize> = Vec::new();
    let mut right: Vec<usize> = Vec::new();
    // What the centre reaches goes right, even when it reaches back.
    for link in &services.links {
        if link.from == centre && link.to != centre && !right.contains(&link.to) {
            right.push(link.to);
        }
    }
    for link in &services.links {
        if link.to == centre && !left.contains(&link.from) && !right.contains(&link.from) {
            left.push(link.from);
        }
    }
    let mut cards = vec![None; services.services.len()];
    let column = |members: &[usize], x: f32, cards: &mut Vec<Option<Rect>>| {
        let height = members.len() as f32 * (CARD.y + GAP) - GAP;
        let top = room.center().y - height / 2.0;
        for (i, &s) in members.iter().enumerate() {
            cards[s] = Some(Rect::from_min_size(
                pos2(x, top + i as f32 * (CARD.y + GAP)),
                CARD,
            ));
        }
    };
    column(&left, room.left(), &mut cards);
    column(&right, room.right() - CARD.x, &mut cards);
    cards[centre] = Some(Rect::from_center_size(room.center(), CARD));
    cards
}

/// Draws the services in `ui`.
pub(crate) fn show(
    ui: &mut Ui,
    view: &mut ServicesView,
    plan: &Plan,
    infra: &mut Infra,
    api: &mut ApiView,
    app: &mut App,
) {
    infra.want();
    infra.poll(plan.map().map(|m| &m.services), app);
    if infra.busy() {
        // Read on threads: look again until they are done.
        ui.ctx().request_repaint();
    }
    let rect = ui.max_rect();
    let background = ui.allocate_rect(
        rect.with_max_x(rect.max.x - crate::plan::EDGE_GRIP),
        Sense::click(),
    );
    let painter = ui.painter_at(rect);
    let corner = rect.left_top() + vec2(24.0, 22.0);
    painter.text(
        corner,
        Align2::LEFT_TOP,
        "Services",
        FontId::proportional(26.0),
        TEXT,
    );
    let Some(map) = plan.map() else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Reading the project…",
            FontId::proportional(16.0),
            DIM,
        );
        ui.ctx().request_repaint();
        crate::plan::switch(ui, rect, app);
        return;
    };
    let combined = infra
        .view()
        .map(|v| ironquill_codemap::with_cloud(&map.services, &v.deployment));
    let services = combined.as_ref().unwrap_or(&map.services);
    let room_top = bar(ui, rect, view, infra);
    if view.mode == Mode::Architecture {
        match infra.view() {
            Some(env) => {
                crate::arch_view::show(ui, rect, room_top, view, &env.deployment, infra, app)
            }
            None => waiting(&painter, rect, infra),
        }
        crate::plan::switch(ui, rect, app);
        return;
    }
    if view.mode == Mode::Configuration {
        configuration(ui, rect, room_top, view, infra, services, app);
        crate::plan::switch(ui, rect, app);
        return;
    }
    if services.services.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "No service found: no Compose file, and no folder serving an API or MCP tools",
            FontId::proportional(16.0),
            DIM,
        );
        crate::plan::switch(ui, rect, app);
        return;
    }
    let centre = view
        .centre
        .filter(|c| *c < services.services.len())
        .or_else(|| settings_centre(infra, services))
        .or(services.centre)
        .unwrap_or(0);
    painter.text(
        corner + vec2(0.0, 34.0),
        Align2::LEFT_TOP,
        format!(
            "{} · {} services · {} links · {} in the middle",
            app.project(),
            services.services.len(),
            services.links.len(),
            services.services[centre].name
        ),
        FontId::proportional(13.0),
        DIM,
    );

    // The details take their room on the right while three columns still
    // fit beside them; narrower, they go over the right column instead.
    let aside = if view.chosen.is_some()
        && rect.width() - DETAILS_WIDTH - 104.0 >= 3.0 * CARD.x + 2.0 * 60.0
    {
        DETAILS_WIDTH + 24.0
    } else {
        0.0
    };
    let room = Rect::from_min_max(
        pos2(rect.min.x + 40.0, room_top + 20.0),
        rect.max - vec2(40.0 + aside, 90.0),
    );
    let cards = arrange(services, centre, room);
    let pointer = background.hover_pos();
    let hovered = pointer.and_then(|p| cards.iter().position(|c| c.is_some_and(|c| c.contains(p))));
    let mut clicked_link: Option<usize> = None;

    // The links: one curve a way, those between the same two services
    // spread apart.
    for (i, link) in services.links.iter().enumerate() {
        let (Some(a), Some(b)) = (cards[link.from], cards[link.to]) else {
            continue;
        };
        let siblings: Vec<usize> = services
            .links
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                (l.from, l.to) == (link.from, link.to) || (l.from, l.to) == (link.to, link.from)
            })
            .map(|(j, _)| j)
            .collect();
        let place = siblings.iter().position(|j| *j == i).unwrap_or(0) as f32;
        let spread = (place - (siblings.len() as f32 - 1.0) / 2.0) * 14.0;
        let rightward = b.center().x >= a.center().x;
        let start = pos2(
            if rightward { a.right() } else { a.left() },
            a.center().y + spread,
        );
        let end = pos2(
            if rightward { b.left() } else { b.right() },
            b.center().y + spread,
        );
        let bend = ((end.x - start.x).abs() * 0.45).max(40.0);
        let sign = if rightward { 1.0 } else { -1.0 };
        let points = [
            start,
            start + vec2(bend * sign, 0.0),
            end - vec2(bend * sign, 0.0),
            end,
        ];
        let (colour, _) = look(link.via);
        let chosen = view.chosen == Some(Chosen::Link(i));
        let focused = matches!(view.chosen, Some(Chosen::Service(s)) if s == link.from || s == link.to)
            || hovered.is_some_and(|h| h == link.from || h == link.to);
        let alpha = if chosen || focused {
            1.0
        } else if view.chosen.is_some() || hovered.is_some() {
            0.25
        } else {
            0.7
        };
        for (w, a) in [(6.0, 0.1), (2.0, 1.0)] {
            painter.add(CubicBezierShape::from_points_stroke(
                points,
                false,
                Color32::TRANSPARENT,
                Stroke::new(
                    if chosen { w * 1.5 } else { w },
                    colour.gamma_multiply(alpha * a),
                ),
            ));
        }
        // The arrow's head, where it arrives.
        let tip = end;
        let back = vec2(-8.0 * sign, 0.0);
        painter.add(egui::Shape::convex_polygon(
            vec![
                tip,
                tip + back + vec2(0.0, -4.5),
                tip + back + vec2(0.0, 4.5),
            ],
            colour.gamma_multiply(alpha),
            Stroke::NONE,
        ));
        // Its label, halfway, a click away.
        let middle = bezier(points, 0.5);
        let text = label(services, i);
        let galley = painter.layout_no_wrap(text, FontId::proportional(11.5), colour);
        let area = Rect::from_center_size(middle, galley.size() + vec2(14.0, 6.0));
        let response = ui
            .interact(area, Id::new(("service-link", i)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        painter.rect(
            area,
            CornerRadius::same(8),
            theme::see(PANEL, 0.95),
            Stroke::new(
                1.0,
                colour.gamma_multiply(if response.hovered() || chosen {
                    1.0
                } else {
                    0.5 * alpha
                }),
            ),
            StrokeKind::Inside,
        );
        painter.galley(area.min + vec2(7.0, 3.0), galley, colour);
        if response.clicked() {
            clicked_link = Some(i);
        }
    }

    // The services.
    let mut clicked_service: Option<usize> = None;
    let mut centred: Option<usize> = None;
    for (s, card) in cards.iter().enumerate() {
        let Some(card) = *card else { continue };
        let service = &services.services[s];
        let picked = view.chosen == Some(Chosen::Service(s)) || hovered == Some(s);
        let response = ui.interact(card, Id::new(("service", s)), Sense::click());
        painter.rect(
            card,
            CornerRadius::same(10),
            theme::see(RAISED, 0.98),
            Stroke::new(
                if s == centre || picked { 1.6 } else { 1.0 },
                if picked {
                    ACCENT
                } else if s == centre {
                    TEXT.gamma_multiply(0.6)
                } else {
                    EDGE
                },
            ),
            StrokeKind::Inside,
        );
        painter.text(
            card.left_top() + vec2(14.0, 12.0),
            Align2::LEFT_TOP,
            &service.name,
            FontId::proportional(16.0),
            TEXT,
        );
        painter.text(
            card.left_top() + vec2(14.0, 36.0),
            Align2::LEFT_TOP,
            subtitle(services, s),
            FontId::proportional(12.0),
            DIM,
        );
        if response.double_clicked() {
            centred = Some(s);
        } else if response.clicked() {
            clicked_service = Some(s);
        }
    }

    // Those not linked to the one in the middle.
    let apart: Vec<&str> = cards
        .iter()
        .enumerate()
        .filter(|(_, c)| c.is_none())
        .map(|(s, _)| services.services[s].name.as_str())
        .collect();
    if !apart.is_empty() {
        painter.text(
            rect.left_bottom() + vec2(24.0, -40.0),
            Align2::LEFT_BOTTOM,
            format!(
                "Not linked to {}: {}",
                services.services[centre].name,
                apart.join(", ")
            ),
            FontId::proportional(12.5),
            DIM,
        );
    }
    painter.text(
        rect.left_bottom() + vec2(24.0, -18.0),
        Align2::LEFT_BOTTOM,
        "click a service or a link for what it holds · double-click a service to put it in the middle · blue OpenAPI · violet MCP · yellow HTTP · cyan cloud",
        FontId::proportional(12.0),
        DIM,
    );

    if let Some(s) = centred {
        view.centre = Some(s);
        view.chosen = Some(Chosen::Service(s));
    } else if let Some(s) = clicked_service {
        view.chosen = Some(Chosen::Service(s));
    } else if let Some(l) = clicked_link {
        view.chosen = Some(Chosen::Link(l));
    } else if background.clicked() {
        view.chosen = None;
    }
    if let Some(chosen) = view.chosen {
        details(ui, rect, view, map, services, infra, chosen, api, app);
    }
    crate::plan::switch(ui, rect, app);
}

/// A point of the cubic Bézier `p` at `t`.
fn bezier(p: [Pos2; 4], t: f32) -> Pos2 {
    let u = 1.0 - t;
    let v = p[0].to_vec2() * u * u * u
        + p[1].to_vec2() * 3.0 * u * u * t
        + p[2].to_vec2() * 3.0 * u * t * t
        + p[3].to_vec2() * t * t * t;
    v.to_pos2()
}

/// A file and a line as a link; returns whether it was clicked.
fn place(ui: &mut Ui, path: &str, line: usize, text: &str) -> bool {
    let shown = ui.add(
        egui::Label::new(
            RichText::new(format!("{path}:{line}"))
                .monospace()
                .size(11.0)
                .color(LINK),
        )
        .sense(Sense::click()),
    );
    if !text.is_empty() {
        ui.label(RichText::new(text).monospace().size(11.0).color(DIM));
    }
    shown
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// What is chosen, whole, on the right: a service or a link.
#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
fn details(
    ui: &mut Ui,
    rect: Rect,
    view: &mut ServicesView,
    map: &CodeMap,
    services: &Services,
    infra: &Infra,
    chosen: Chosen,
    api: &mut ApiView,
    app: &mut App,
) {
    let mut open_place: Option<Place> = None;
    let top = rect.right_top() + vec2(-DETAILS_WIDTH - 24.0, 104.0);
    let height = (rect.bottom() - 70.0 - top.y).max(120.0);
    let mut open: Option<(String, usize)> = None;
    let mut route: Option<usize> = None;
    let mut choose: Option<Chosen> = None;
    let mut centre = false;
    let mut close = false;
    egui::Area::new(Id::new("services-details"))
        .fixed_pos(top)
        .show(ui.ctx(), |ui| {
            Frame::new()
                .fill(theme::see(PANEL, 0.96))
                .stroke(Stroke::new(1.0, EDGE))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(14))
                .show(ui, |ui| {
                    ui.set_width(DETAILS_WIDTH - 28.0);
                    ui.set_max_height(height);
                    ScrollArea::vertical().id_salt("services-details").show(ui, |ui| match chosen {
                        Chosen::Service(s) => {
                            let service = &services.services[s];
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&service.name).size(19.0).color(TEXT));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    close = ui.small_button("close").clicked();
                                    centre = ui.small_button("Put it in the middle").clicked();
                                });
                            });
                            ui.label(RichText::new(subtitle(services, s)).size(12.0).color(DIM));
                            if !service.operations.is_empty() {
                                let (called, alone): (Vec<usize>, Vec<usize>) = service
                                    .operations
                                    .iter()
                                    .partition(|&&o| !map.operations[o].callers.is_empty());
                                ui.add_space(8.0);
                                ui.label(RichText::new(format!(
                                    "ROUTES · {} called from the project · {} with no caller found",
                                    called.len(),
                                    alone.len()
                                )).size(11.0).color(DIM));
                                for o in alone.iter().take(40) {
                                    let operation = &map.operations[*o];
                                    ui.horizontal(|ui| {
                                        api_view::method_pill(ui, &operation.method, 10.5);
                                        let shown = ui.add(
                                            egui::Label::new(RichText::new(&operation.path).monospace().size(11.5).color(TEXT))
                                                .sense(Sense::click()),
                                        );
                                        if shown.on_hover_text("open the function that serves it").clicked()
                                            && let Some((node, line)) = operation.handler
                                        {
                                            open = Some((map.nodes[node].path.clone(), line));
                                        }
                                    });
                                }
                                if alone.len() > 40 {
                                    ui.label(RichText::new(format!("and {} more", alone.len() - 40)).size(11.0).color(DIM));
                                }
                                if ui.small_button("the API view").clicked() {
                                    route = service.operations.first().copied();
                                }
                            }
                            if !service.tools.is_empty() {
                                ui.add_space(8.0);
                                ui.label(RichText::new("MCP TOOLS AND PROMPTS").size(11.0).color(DIM));
                                for tool in &service.tools {
                                    ui.horizontal(|ui| {
                                        let kind = match tool.kind {
                                            ToolKind::Tool => "tool",
                                            ToolKind::Prompt => "prompt",
                                        };
                                        ui.label(RichText::new(kind).size(10.5).color(MAGENTA));
                                        let shown = ui.add(
                                            egui::Label::new(RichText::new(&tool.name).monospace().color(TEXT))
                                                .sense(Sense::click()),
                                        );
                                        if shown.on_hover_text(format!("{}:{}", tool.path, tool.line)).clicked() {
                                            open = Some((tool.path.clone(), tool.line));
                                        }
                                    });
                                    if !tool.summary.is_empty() {
                                        ui.label(RichText::new(&tool.summary).size(11.5).color(DIM));
                                    }
                                }
                            }
                            for (title, incoming) in [("REACHED BY", true), ("REACHES", false)] {
                                let links: Vec<usize> = services
                                    .links
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, l)| if incoming { l.to == s } else { l.from == s })
                                    .map(|(i, _)| i)
                                    .collect();
                                if links.is_empty() {
                                    continue;
                                }
                                ui.add_space(8.0);
                                ui.label(RichText::new(title).size(11.0).color(DIM));
                                for l in links {
                                    let link = &services.links[l];
                                    let other = if incoming { link.from } else { link.to };
                                    let (colour, _) = look(link.via);
                                    let text = format!("{} · {}", services.services[other].name, label(services, l));
                                    let shown = ui.add(
                                        egui::Label::new(RichText::new(text).size(12.5).color(colour)).sense(Sense::click()),
                                    );
                                    if shown.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                        choose = Some(Chosen::Link(l));
                                    }
                                }
                            }
                        }
                        Chosen::Link(l) => {
                            let link = &services.links[l];
                            let (colour, way) = look(link.via);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!(
                                    "{} → {}",
                                    services.services[link.from].name,
                                    services.services[link.to].name
                                )).size(18.0).color(TEXT));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    close = ui.small_button("close").clicked();
                                });
                            });
                            ui.label(RichText::new(way).size(12.0).color(colour));
                            if link.via == Via::Cloud
                                && let Some(element) = infra.view().and_then(|v| {
                                    v.deployment.elements.iter().find(|e| {
                                        Some(e.kind) == services.services[link.to].cloud
                                            && e.name == services.services[link.to].name
                                    })
                                })
                                && let Some(p) = crate::arch_view::element_details(ui, element)
                            {
                                open_place = Some(p);
                            }
                            if !link.operations.is_empty() {
                                ui.add_space(8.0);
                                ui.label(RichText::new("ROUTES CALLED").size(11.0).color(DIM));
                                for &o in &link.operations {
                                    let operation = &map.operations[o];
                                    ui.horizontal(|ui| {
                                        api_view::method_pill(ui, &operation.method, 10.5);
                                        let shown = ui.add(
                                            egui::Label::new(RichText::new(&operation.path).monospace().size(11.5).color(TEXT))
                                                .sense(Sense::click()),
                                        );
                                        if shown.on_hover_text("open it in the API view").clicked() {
                                            route = Some(o);
                                        }
                                    });
                                }
                            }
                            if link.via == Via::Mcp {
                                let tools = &services.services[link.to].tools;
                                ui.add_space(8.0);
                                ui.label(RichText::new("TOOLS OFFERED").size(11.0).color(DIM));
                                ui.label(RichText::new("the model picks among them as it runs: which it calls is not in the code").size(11.0).color(DIM));
                                for tool in tools {
                                    let shown = ui.add(
                                        egui::Label::new(RichText::new(&tool.name).monospace().color(TEXT)).sense(Sense::click()),
                                    );
                                    if shown.clicked() {
                                        open = Some((tool.path.clone(), tool.line));
                                    }
                                }
                                if tools.is_empty() {
                                    ui.label(RichText::new("none read: outside the project, or declared otherwise").size(11.5).color(DIM));
                                }
                            }
                            ui.add_space(8.0);
                            ui.label(RichText::new(format!("READ IN {} PLACE{}", link.evidence.len(), if link.evidence.len() == 1 { "" } else { "S" })).size(11.0).color(DIM));
                            for evidence in &link.evidence {
                                if place(ui, &evidence.path, evidence.line, &evidence.text) {
                                    open = Some((evidence.path.clone(), evidence.line));
                                }
                                ui.add_space(4.0);
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
    if centre && let Chosen::Service(s) = chosen {
        view.centre = Some(s);
    }
    if let Some((path, line)) = open {
        let path = app.root().join(path);
        app.show_map(None);
        app.open_path_at(path, line);
    }
    if let Some(place) = open_place {
        open_at(infra, &place, app);
    }
    if let Some(o) = route {
        let operation = &map.operations[o];
        api.choose(&map.operations, &operation.method, &operation.path);
        app.show_map(Some(MapView::Api));
    }
}

/// Opens the file a place names, at its line.
pub(crate) fn open_at(infra: &Infra, place: &Place, app: &mut App) {
    if let Some(path) = infra.file(place) {
        app.show_map(None);
        app.open_path_at(path, place.line.max(1));
    }
}

/// The service the settings put in the middle.
fn settings_centre(infra: &Infra, services: &Services) -> Option<usize> {
    let name = infra.read()?.settings.centre.as_deref()?;
    services.services.iter().position(|s| s.name == name)
}

/// While the environment renders, or when there is none.
fn waiting(painter: &egui::Painter, rect: Rect, infra: &Infra) {
    let text = match (infra.read(), infra.rendering()) {
        (_, Some(name)) => format!("Rendering {name} with kustomize and helm…"),
        (None, None) if infra.busy() => "Reading the deployment…".to_owned(),
        (Some(read), None) if read.deploy.environments.is_empty() => {
            "No environment found: no Kustomize tree in the project or in the repositories its settings link".to_owned()
        }
        _ => "Choose an environment".to_owned(),
    };
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(16.0),
        DIM,
    );
}

/// The bar under the title: the environment, what is shown of it, the
/// repositories' update, what could not be read, and a banner for the
/// linked repositories not on their main branch. Returns where the room
/// below starts.
fn bar(ui: &mut Ui, rect: Rect, view: &mut ServicesView, infra: &mut Infra) -> f32 {
    let top = rect.top() + 72.0;
    let area = Rect::from_min_size(
        pos2(rect.left() + 24.0, top),
        vec2(rect.width() - 560.0, 30.0),
    );
    let mut chosen_env: Option<usize> = None;
    let mut update = false;
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(area)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            if let Some(read) = infra.read() {
                let environments = &read.deploy.environments;
                let current = infra
                    .environment
                    .and_then(|e| environments.get(e))
                    .map_or("no environment", |e| e.name.as_str());
                egui::ComboBox::from_id_salt("environment")
                    .selected_text(RichText::new(current).size(13.5).color(TEXT))
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        let mut other = false;
                        for (i, env) in environments.iter().enumerate() {
                            if !env.chosen && !other {
                                other = true;
                                if i > 0 {
                                    ui.separator();
                                }
                                ui.label(RichText::new("Other").size(11.0).color(DIM));
                            }
                            if ui
                                .selectable_label(infra.environment == Some(i), &env.name)
                                .clicked()
                            {
                                chosen_env = Some(i);
                            }
                        }
                    });
            } else if infra.error.is_none() {
                ui.label(RichText::new("Reading the deployment…").size(13.0).color(DIM));
            }
            ui.add_space(12.0);
            for (name, mode) in [
                ("Links", Mode::Links),
                ("Architecture", Mode::Architecture),
                ("Configuration", Mode::Configuration),
            ] {
                let on = view.mode == mode;
                let text = RichText::new(name).size(13.5).color(if on { TEXT } else { DIM });
                if ui.add(egui::Button::selectable(on, text)).clicked() {
                    view.mode = mode;
                }
            }
            let ready = infra.view().is_some();
            if ui
                .add_enabled(ready, egui::Button::new(RichText::new("3D").size(13.5)))
                .on_hover_text("the environment's architecture in 3D, in the universe window")
                .on_disabled_hover_text("once the environment is rendered")
                .clicked()
            {
                view.open_3d = true;
            }
            ui.add_space(12.0);
            let linked = infra
                .read()
                .is_some_and(|r| r.repos.repos.iter().any(|repo| !repo.own));
            let label = if infra.updating() { "Updating…" } else { "Update repos" };
            if ui
                .add_enabled(linked && !infra.updating(), egui::Button::new(RichText::new(label).size(13.0)))
                .on_hover_text("fetch each linked repository, and fast-forward those on their main branch with no local change; the project's own is never touched")
                .on_disabled_hover_text("the settings link no repository")
                .clicked()
            {
                update = true;
            }
            let notes = infra.read().map_or(0, |r| r.notes.len())
                + infra.view().map_or(0, |v| v.deployment.notes.len());
            if notes > 0
                && ui
                    .add(egui::Button::selectable(
                        view.notes,
                        RichText::new(format!("{notes} note{}", if notes == 1 { "" } else { "s" }))
                            .size(13.0)
                            .color(YELLOW),
                    ))
                    .clicked()
            {
                view.notes = !view.notes;
            }
            if let Some(name) = infra.rendering() {
                ui.label(RichText::new(format!("rendering {name}…")).size(12.5).color(DIM));
            }
        },
    );
    if let Some(i) = chosen_env {
        infra.choose(i);
        view.chosen = None;
        view.element = None;
        view.release = None;
    }
    if update {
        infra.update_repos();
    }
    let painter = ui.painter_at(rect);
    let mut y = top + 40.0;
    if let Some(error) = &infra.error {
        painter.text(
            pos2(rect.left() + 24.0, y),
            Align2::LEFT_TOP,
            format!("The settings could not be read: {error}"),
            FontId::proportional(13.0),
            theme::RED,
        );
        y += 22.0;
    } else if let Some(read) = infra.read()
        && read.settings.linked.is_empty()
        && read.deploy.environments.is_empty()
    {
        let file = infra
            .settings_file()
            .map_or_else(String::new, |f| f.display().to_string());
        painter.text(
            pos2(rect.left() + 24.0, y),
            Align2::LEFT_TOP,
            format!("To read where it runs, link its deployment repositories in {file}: linked = [\"~/code/deploy\"]"),
            FontId::proportional(12.5),
            DIM,
        );
        y += 22.0;
    }
    if let Some(read) = infra.read() {
        for (repo, branch) in &read.off_main {
            let banner = Rect::from_min_size(
                pos2(rect.left() + 24.0, y),
                vec2(rect.width() - 560.0, 24.0),
            );
            painter.rect_filled(banner, CornerRadius::same(6), YELLOW.gamma_multiply(0.15));
            painter.text(
                banner.left_center() + vec2(10.0, 0.0),
                Align2::LEFT_CENTER,
                format!(
                    "{repo} is on {branch}, not its main branch: what shows is read from {branch}"
                ),
                FontId::proportional(12.5),
                YELLOW,
            );
            y += 30.0;
        }
    }
    if view.notes {
        let mut notes: Vec<String> = infra.read().map(|r| r.notes.clone()).unwrap_or_default();
        if let Some(v) = infra.view() {
            notes.extend(v.deployment.notes.iter().cloned());
        }
        egui::Window::new("What could not be read")
            .id(Id::new("deployment-notes"))
            .collapsible(false)
            .default_pos(pos2(rect.left() + 40.0, y + 10.0))
            .default_width(560.0)
            .open(&mut view.notes)
            .show(ui.ctx(), |ui| {
                ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    for note in &notes {
                        ui.label(RichText::new(note).size(12.5).color(TEXT));
                        ui.add_space(4.0);
                    }
                });
            });
    }
    y
}

/// A release's settings, layer by layer: each with its value and where it
/// was set, those replaced under it; a filter; a double click opens the
/// file at the line.
fn configuration(
    ui: &mut Ui,
    rect: Rect,
    top: f32,
    view: &mut ServicesView,
    infra: &Infra,
    services: &Services,
    app: &mut App,
) {
    let painter = ui.painter_at(rect);
    let Some(env) = infra
        .read()
        .and_then(|r| r.deploy.environments.get(infra.environment?))
    else {
        waiting(&painter, rect, infra);
        return;
    };
    if env.releases.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            format!("{} deploys no Helm release", env.name),
            FontId::proportional(16.0),
            DIM,
        );
        return;
    }
    // The release of the service in the middle, at first.
    let centre = view
        .centre
        .and_then(|c| services.services.get(c))
        .map(|s| s.name.clone())
        .or_else(|| infra.read()?.settings.centre.clone());
    let release = view
        .release
        .as_deref()
        .and_then(|l| env.releases.iter().find(|r| r.label == l))
        .or_else(|| {
            env.releases
                .iter()
                .find(|r| Some(&r.name) == centre.as_ref())
        })
        .unwrap_or(&env.releases[0]);
    let area = Rect::from_min_max(
        pos2(rect.left() + 24.0, top + 10.0),
        rect.max - vec2(40.0, 60.0),
    );
    let mut open: Option<Place> = None;
    let mut pick: Option<String> = None;
    let mut toggle: Option<String> = None;
    ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("release")
                .selected_text(RichText::new(&release.label).size(15.0).color(TEXT))
                .width(220.0)
                .show_ui(ui, |ui| {
                    for r in &env.releases {
                        if ui.selectable_label(r.label == release.label, &r.label).clicked() {
                            pick = Some(r.label.clone());
                        }
                    }
                });
            ui.add_space(10.0);
            ui.label(RichText::new("filter").size(12.5).color(DIM));
            ui.add(
                egui::TextEdit::singleline(&mut view.filter)
                    .hint_text("a key or a value")
                    .desired_width(240.0),
            );
            let chart = release.chart_name.as_deref().unwrap_or("an unknown chart");
            let source = release.source.as_ref().map_or_else(String::new, |s| {
                format!(
                    " from {} {}{}",
                    s.kind,
                    s.url.as_deref().unwrap_or(&s.name),
                    s.reference.as_deref().map_or_else(String::new, |r| format!(" at {r}"))
                )
            });
            ui.label(RichText::new(format!("chart {chart}{source}")).size(12.0).color(DIM));
        });
        if ui
            .add(
                egui::Label::new(
                    RichText::new(format!("declared at {}", release.place))
                        .monospace()
                        .size(11.0)
                        .color(LINK),
                )
                .sense(Sense::click()),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            open = Some(release.place.clone());
        }
        for note in &release.notes {
            ui.label(RichText::new(note).size(12.0).color(YELLOW));
        }
        ui.add_space(6.0);
        let filter = view.filter.to_lowercase();
        let shown: Vec<&ironquill_codemap::Setting> = release
            .settings
            .iter()
            .filter(|s| {
                filter.is_empty()
                    || s.key.to_lowercase().contains(&filter)
                    || s.value.to_lowercase().contains(&filter)
            })
            .collect();
        ui.label(
            RichText::new(format!(
                "{} SETTINGS{} · a click shows what each replaced, a double click opens where it is set",
                shown.len(),
                if filter.is_empty() { String::new() } else { format!(" OF {}", release.settings.len()) }
            ))
            .size(11.0)
            .color(DIM),
        );
        ScrollArea::both().id_salt("configuration").show(ui, |ui| {
            egui::Grid::new("settings")
                .num_columns(3)
                .spacing(vec2(18.0, 4.0))
                .striped(true)
                .show(ui, |ui| {
                    for setting in shown {
                        let layers = setting.replaced.len();
                        let key = ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "{}{}",
                                    if layers == 0 {
                                        "  "
                                    } else if view.expanded.contains(&setting.key) {
                                        "▾ "
                                    } else {
                                        "▸ "
                                    },
                                    setting.key
                                ))
                                .monospace()
                                .size(12.0)
                                .color(TEXT),
                            )
                            .sense(Sense::click()),
                        );
let value = ui
                            .allocate_ui_with_layout(
                                vec2(VALUE_WIDTH, 16.0),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.set_min_width(VALUE_WIDTH);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&setting.value)
                                                .monospace()
                                                .size(12.0)
                                                .color(CODE),
                                        )
                                        .truncate()
                                        .sense(Sense::click()),
                                    )
                                    .on_hover_text(&setting.value)
                                },
                            )
                            .inner;
                        let at = ui.add(
                            egui::Label::new(
                                RichText::new(setting.place.to_string()).monospace().size(11.0).color(LINK),
                            )
                            .sense(Sense::click()),
                        );
                        ui.end_row();
                        if key.double_clicked() || value.double_clicked() || at.double_clicked() || at.clicked() {
                            open = Some(setting.place.clone());
                        } else if (key.clicked() || value.clicked()) && layers > 0 {
                            toggle = Some(setting.key.clone());
                        }
                        if view.expanded.contains(&setting.key) {
                            for earlier in &setting.replaced {
                                ui.label(RichText::new("    replaced").size(11.0).color(DIM));
                                ui.label(RichText::new(&earlier.value).monospace().size(11.5).color(DIM));
                                let at = ui.add(
                                    egui::Label::new(
                                        RichText::new(earlier.place.to_string()).monospace().size(11.0).color(LINK.gamma_multiply(0.7)),
                                    )
                                    .sense(Sense::click()),
                                );
                                ui.end_row();
                                if at.clicked() || at.double_clicked() {
                                    open = Some(earlier.place.clone());
                                }
                            }
                        }
                    }
                });
            // What the tools rendered of it: each object, where it was
            // declared and what changed it after.
            if let Some(rendered) = infra.view().map(|v| &v.rendered) {
                let objects: Vec<&ironquill_codemap::Object> = rendered
                    .objects
                    .iter()
                    .filter(|o| o.service.as_deref() == Some(release.label.as_str()))
                    .collect();
                if !objects.is_empty() {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(format!("{} OBJECTS RENDERED", objects.len()))
                            .size(11.0)
                            .color(DIM),
                    );
                    for object in objects {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new(format!("{} {}", object.kind, object.name))
                                    .monospace()
                                    .size(12.0)
                                    .color(TEXT),
                            );
                            for (word, place) in object
                                .origin
                                .iter()
                                .map(|p| ("from", p))
                                .chain(object.changed_by.iter().map(|p| ("changed by", p)))
                            {
                                ui.label(RichText::new(word).size(11.0).color(DIM));
                                let at = ui.add(
                                    egui::Label::new(
                                        RichText::new(place.to_string())
                                            .monospace()
                                            .size(11.0)
                                            .color(LINK),
                                    )
                                    .sense(Sense::click()),
                                );
                                if at.clicked() {
                                    open = Some(place.clone());
                                }
                            }
                        });
                    }
                }
            }
        });
    });
    if let Some(label) = pick {
        view.release = Some(label);
    }
    if let Some(key) = toggle
        && !view.expanded.remove(&key)
    {
        view.expanded.insert(key);
    }
    if let Some(place) = open {
        open_at(infra, &place, app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironquill_codemap::{Service, ServiceLink};

    fn service(name: &str) -> Service {
        Service {
            name: name.into(),
            folder: Some(name.into()),
            external: false,
            operations: Vec::new(),
            tools: Vec::new(),
            cloud: None,
        }
    }

    fn link(from: usize, to: usize, via: Via) -> ServiceLink {
        ServiceLink {
            from,
            to,
            via,
            operations: Vec::new(),
            evidence: Vec::new(),
        }
    }

    #[test]
    fn callers_on_the_left_called_on_the_right_the_rest_apart() {
        let services = Services {
            services: ["web", "api", "db", "tools", "apart"].map(service).to_vec(),
            links: vec![
                link(0, 1, Via::OpenApi),
                link(1, 2, Via::Http),
                link(1, 3, Via::Mcp),
                link(3, 1, Via::Http),
            ],
            centre: Some(1),
        };
        let room = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 600.0));
        let cards = arrange(&services, 1, room);
        let x = |s: usize| cards[s].unwrap().center().x;
        assert!((x(1) - 500.0).abs() < 1.0);
        assert!(x(0) < x(1), "web reaches the api: on the left");
        assert!(
            x(2) > x(1) && x(3) > x(1),
            "what the api reaches: on the right"
        );
        assert!(cards[4].is_none(), "not linked: apart");
        assert_eq!(label(&services, 0), "OpenAPI · 0 routes");
        assert_eq!(label(&services, 2), "MCP");
        assert_eq!(subtitle(&services, 4), "apart/");
    }
}
