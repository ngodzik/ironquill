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
use ironquill_codemap::{CodeMap, Services, ToolKind, Via};
use ironquill_ui::{App, MapView};

use crate::api_view::{self, ApiView};
use crate::plan::Plan;
use crate::theme::{self, ACCENT, DIM, EDGE, LINK, MAGENTA, PANEL, RAISED, TEXT, YELLOW};

/// A card's size, and the room between cards of a column.
const CARD: egui::Vec2 = egui::Vec2::new(220.0, 64.0);
const GAP: f32 = 18.0;

/// The details' width, on the right.
const DETAILS_WIDTH: f32 = 380.0;

/// What is chosen: a service, or a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chosen {
    Service(usize),
    Link(usize),
}

/// What the services view remembers between frames.
#[derive(Default)]
pub(crate) struct ServicesView {
    /// The service put in the middle, when not the map's centre.
    centre: Option<usize>,
    chosen: Option<Chosen>,
}

/// A way's colour and name.
fn look(via: Via) -> (Color32, &'static str) {
    match via {
        Via::OpenApi => (LINK, "OpenAPI"),
        Via::Mcp => (MAGENTA, "MCP"),
        Via::Http => (YELLOW, "HTTP"),
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
    }
}

/// What a card says under a service's name.
fn subtitle(services: &Services, service: usize) -> String {
    let s = &services.services[service];
    if s.external {
        return "outside the project".to_owned();
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
    api: &mut ApiView,
    app: &mut App,
) {
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
    let services = &map.services;
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

    let aside = if view.chosen.is_some() {
        DETAILS_WIDTH + 24.0
    } else {
        0.0
    };
    let room = Rect::from_min_max(
        rect.min + vec2(40.0, 120.0),
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
        "click a service or a link for what it holds · double-click a service to put it in the middle · blue OpenAPI · violet MCP · yellow HTTP",
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
        details(ui, rect, view, map, chosen, api, app);
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
fn details(
    ui: &mut Ui,
    rect: Rect,
    view: &mut ServicesView,
    map: &CodeMap,
    chosen: Chosen,
    api: &mut ApiView,
    app: &mut App,
) {
    let services = &map.services;
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
    if let Some(o) = route {
        let operation = &map.operations[o];
        api.choose(&map.operations, &operation.method, &operation.path);
        app.show_map(Some(MapView::Api));
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
