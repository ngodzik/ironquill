//! An environment's architecture, flat: the way in on the left, the
//! cluster's workloads and jobs by namespace in the middle, inside the
//! network, the cloud resources they use on the right, and the services
//! outside beyond. A click shows what is known of an element and where it
//! is configured; a double click opens that file.

use bevy_egui::egui::epaint::CubicBezierShape;
use bevy_egui::egui::{
    self, Align2, Color32, CornerRadius, FontId, Frame, Id, Margin, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};
use ironquill_codemap::{Deployment, Element, ElementKind, LinkKind, Place};
use ironquill_ui::App;

use crate::infra::Infra;
use crate::services_view::{ServicesView, open_at};
use crate::theme::{
    self, ACCENT, CYAN, DIM, EDGE, GREEN, LINK, MAGENTA, PANEL, RAISED, TEXT, YELLOW,
};

const CARD: egui::Vec2 = egui::Vec2::new(210.0, 50.0);
const GAP: f32 = 12.0;
const DETAILS_WIDTH: f32 = 380.0;

/// The colour of a kind of element.
pub(crate) fn colour(kind: ElementKind) -> Color32 {
    match kind {
        ElementKind::Internet | ElementKind::Dns => DIM,
        ElementKind::Firewall => theme::RED,
        ElementKind::LoadBalancer => YELLOW,
        ElementKind::Network | ElementKind::Cluster => EDGE,
        ElementKind::Workload => ACCENT,
        ElementKind::Job => YELLOW,
        ElementKind::Database => LINK,
        ElementKind::Bucket => GREEN,
        ElementKind::Secret => MAGENTA,
        ElementKind::Role => CYAN,
        ElementKind::Users => GREEN,
        ElementKind::External => TEXT,
    }
}

/// The colour of a kind of link.
fn link_colour(kind: LinkKind) -> Color32 {
    match kind {
        LinkKind::Routes => YELLOW,
        LinkKind::Calls => LINK,
        LinkKind::Uses => CYAN,
        LinkKind::Assumes => MAGENTA,
        LinkKind::Contains => EDGE,
    }
}

/// The column an element stands in.
fn column(kind: ElementKind) -> Option<usize> {
    match kind {
        ElementKind::Internet
        | ElementKind::Dns
        | ElementKind::Firewall
        | ElementKind::LoadBalancer => Some(0),
        // The databases under the cluster, inside the network.
        ElementKind::Workload | ElementKind::Job | ElementKind::Database => Some(1),
        ElementKind::Bucket | ElementKind::Secret | ElementKind::Role | ElementKind::Users => {
            Some(2)
        }
        ElementKind::External => Some(3),
        ElementKind::Network | ElementKind::Cluster => None,
    }
}

/// Where each element's card is: in its column, in the order of its
/// kind, workloads by namespace.
fn arrange(deployment: &Deployment, room: Rect) -> Vec<Option<Rect>> {
    let mut columns: [Vec<usize>; 4] = Default::default();
    let mut order: Vec<usize> = (0..deployment.elements.len()).collect();
    order.sort_by_key(|&i| {
        let e = &deployment.elements[i];
        (e.kind, e.namespace.clone(), e.name.clone())
    });
    for i in order {
        if let Some(c) = column(deployment.elements[i].kind) {
            columns[c].push(i);
        }
    }
    let used: Vec<usize> = (0..4).filter(|c| !columns[*c].is_empty()).collect();
    let mut cards = vec![None; deployment.elements.len()];
    // Narrow, the cards narrow too, down to what still reads.
    let n = used.len().max(1) as f32;
    let width = ((room.width() - 50.0 * (n - 1.0)) / n).clamp(140.0, CARD.x);
    let step = if used.len() > 1 {
        (room.width() - width) / (n - 1.0)
    } else {
        0.0
    };
    for (n, &c) in used.iter().enumerate() {
        let members = &columns[c];
        let x = if used.len() == 1 {
            room.center().x - width / 2.0
        } else {
            room.left() + step * n as f32
        };
        let height = members.len() as f32 * (CARD.y + GAP) - GAP;
        let top = (room.center().y - height / 2.0).max(room.top() + 24.0);
        for (k, &i) in members.iter().enumerate() {
            cards[i] = Some(Rect::from_min_size(
                pos2(x, top + k as f32 * (CARD.y + GAP)),
                vec2(width, CARD.y),
            ));
        }
    }
    cards
}

fn bezier(p: [Pos2; 4], t: f32) -> Pos2 {
    let u = 1.0 - t;
    (p[0].to_vec2() * u * u * u
        + p[1].to_vec2() * 3.0 * u * u * t
        + p[2].to_vec2() * 3.0 * u * t * t
        + p[3].to_vec2() * t * t * t)
        .to_pos2()
}

/// Draws `deployment` below `top` in `rect`.
pub(crate) fn show(
    ui: &mut Ui,
    rect: Rect,
    top: f32,
    view: &mut ServicesView,
    deployment: &Deployment,
    infra: &Infra,
    app: &mut App,
) {
    let painter = ui.painter_at(rect);
    let aside = if view.element.is_some() {
        DETAILS_WIDTH + 24.0
    } else {
        0.0
    };
    let room = Rect::from_min_max(
        pos2(rect.left() + 40.0, top + 30.0),
        rect.max - vec2(40.0 + aside, 60.0),
    );
    let cards = arrange(deployment, room);
    let pointer = ui.ctx().pointer_hover_pos();
    let hovered = pointer.and_then(|p| cards.iter().position(|c| c.is_some_and(|c| c.contains(p))));

    // The network around the cluster and the databases; the cluster
    // around its workloads.
    let around = |kinds: &[ElementKind]| -> Option<Rect> {
        deployment
            .elements
            .iter()
            .zip(&cards)
            .filter(|(e, _)| kinds.contains(&e.kind))
            .filter_map(|(_, c)| *c)
            .reduce(|a, b| a.union(b))
            .map(|r| r.expand(22.0))
    };
    for (kind, inside) in [
        (
            ElementKind::Network,
            &[
                ElementKind::Workload,
                ElementKind::Job,
                ElementKind::Database,
            ][..],
        ),
        (
            ElementKind::Cluster,
            &[ElementKind::Workload, ElementKind::Job][..],
        ),
    ] {
        let Some((_, element)) = deployment.of(kind).next() else {
            continue;
        };
        let Some(mut frame) = around(inside) else {
            continue;
        };
        if kind == ElementKind::Network {
            frame = frame.expand(14.0);
        }
        painter.rect(
            frame,
            CornerRadius::same(14),
            theme::see(
                if kind == ElementKind::Network {
                    PANEL
                } else {
                    RAISED
                },
                0.5,
            ),
            Stroke::new(1.0, EDGE),
            StrokeKind::Inside,
        );
        // The network's name above it, the cluster's inside its frame.
        let at = if kind == ElementKind::Network {
            frame.left_top() + vec2(12.0, -16.0)
        } else {
            frame.left_top() + vec2(12.0, 4.0)
        };
        painter.text(
            at,
            Align2::LEFT_TOP,
            format!("{} · {}", kind.label(), element.name),
            FontId::proportional(12.0),
            DIM,
        );
    }

    for link in &deployment.links {
        if link.kind == LinkKind::Contains {
            continue;
        }
        let (Some(a), Some(b)) = (cards[link.from], cards[link.to]) else {
            continue;
        };
        // In one column, straight down or up; across, a curve.
        let points = if (a.center().x - b.center().x).abs() < 1.0 {
            let (start, end) = if b.center().y >= a.center().y {
                (a.center_bottom(), b.center_top())
            } else {
                (a.center_top(), b.center_bottom())
            };
            let side = vec2(a.width() * 0.35, 0.0);
            let (start, end) = if (b.center().y - a.center().y).abs() > CARD.y + GAP + 1.0 {
                // Past the cards between: along their right side.
                (a.right_center(), b.right_center())
            } else {
                (start, end)
            };
            if start.x > a.center().x {
                [start, start + side, end + side, end]
            } else {
                [start, start.lerp(end, 0.33), start.lerp(end, 0.66), end]
            }
        } else {
            let rightward = b.center().x >= a.center().x;
            let start = if rightward {
                a.right_center()
            } else {
                a.left_center()
            };
            let end = if rightward {
                b.left_center()
            } else {
                b.right_center()
            };
            let bend =
                ((end.x - start.x).abs() * 0.45).max(30.0) * if rightward { 1.0 } else { -1.0 };
            [start, start + vec2(bend, 0.0), end - vec2(bend, 0.0), end]
        };
        let focused = view.element.is_some_and(|e| e == link.from || e == link.to)
            || hovered.is_some_and(|h| h == link.from || h == link.to);
        let alpha = if focused {
            1.0
        } else if view.element.is_some() || hovered.is_some() {
            0.2
        } else {
            0.6
        };
        let colour = link_colour(link.kind);
        painter.add(CubicBezierShape::from_points_stroke(
            points,
            false,
            Color32::TRANSPARENT,
            Stroke::new(
                if focused { 2.2 } else { 1.4 },
                colour.gamma_multiply(alpha),
            ),
        ));
        if focused && !link.label.is_empty() {
            painter.text(
                bezier(points, 0.5),
                Align2::CENTER_BOTTOM,
                &link.label,
                FontId::proportional(11.0),
                colour,
            );
        }
    }

    let mut clicked = None;
    let mut opened = None;
    for (i, card) in cards.iter().enumerate() {
        let Some(card) = *card else { continue };
        let element = &deployment.elements[i];
        let picked = view.element == Some(i) || hovered == Some(i);
        let response = ui.interact(card, Id::new(("element", i)), Sense::click());
        let colour = colour(element.kind);
        painter.rect(
            card,
            CornerRadius::same(9),
            theme::see(RAISED, 0.98),
            Stroke::new(
                if picked { 1.6 } else { 1.0 },
                if picked {
                    ACCENT
                } else {
                    colour.gamma_multiply(0.6)
                },
            ),
            StrokeKind::Inside,
        );
        painter.rect_filled(
            Rect::from_min_size(card.min, vec2(4.0, card.height())),
            CornerRadius::same(2),
            colour,
        );
        let name = match &element.namespace {
            Some(ns)
                if element.kind == ElementKind::Workload || element.kind == ElementKind::Job =>
            {
                format!("{} · {ns}", element.name)
            }
            _ => element.name.clone(),
        };
        painter.text(
            card.left_top() + vec2(14.0, 9.0),
            Align2::LEFT_TOP,
            clip(&name, (card.width() / 8.2) as usize),
            FontId::proportional(14.0),
            TEXT,
        );
        let mut under = element.kind.label().to_owned();
        if let Some((min, max)) = element.replicas {
            under = if min == max {
                format!("{under} · ×{min}")
            } else {
                format!("{under} · ×{min} to {max}")
            };
        }
        painter.text(
            card.left_top() + vec2(14.0, 29.0),
            Align2::LEFT_TOP,
            under,
            FontId::proportional(11.5),
            DIM,
        );
        if response.double_clicked() {
            opened = element.places.first().cloned();
        } else if response.clicked() {
            clicked = Some(i);
        }
    }
    painter.text(
        rect.left_bottom() + vec2(24.0, -18.0),
        Align2::LEFT_BOTTOM,
        format!(
            "{} · {} elements · click one for what is known of it, double-click to open where it is configured · yellow routes · blue calls · cyan uses · violet assumes",
            deployment.environment,
            deployment.elements.len()
        ),
        FontId::proportional(12.0),
        DIM,
    );
    if let Some(i) = clicked {
        view.element = Some(i);
    }
    if let Some(i) = view.element.filter(|i| *i < deployment.elements.len()) {
        let top_left = rect.right_top() + vec2(-DETAILS_WIDTH - 24.0, top - rect.top() + 20.0);
        let height = (rect.bottom() - 70.0 - top_left.y).max(120.0);
        let mut close = false;
        egui::Area::new(Id::new("element-details"))
            .fixed_pos(top_left)
            .show(ui.ctx(), |ui| {
                Frame::new()
                    .fill(theme::see(PANEL, 0.96))
                    .stroke(Stroke::new(1.0, EDGE))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_width(DETAILS_WIDTH - 28.0);
                        ui.set_max_height(height);
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    close = ui.small_button("close").clicked();
                                },
                            );
                        });
                        ScrollArea::vertical()
                            .id_salt("element-details")
                            .show(ui, |ui| {
                                if let Some(p) = element_details(ui, &deployment.elements[i]) {
                                    opened = Some(p);
                                }
                                let related: Vec<String> = deployment
                                    .links
                                    .iter()
                                    .filter(|l| l.from == i || l.to == i)
                                    .map(|l| {
                                        let (word, other) = if l.from == i {
                                            (format!("{:?}", l.kind).to_lowercase(), l.to)
                                        } else {
                                            (format!("{:?} by", l.kind).to_lowercase(), l.from)
                                        };
                                        format!("{word} {}", deployment.elements[other].name)
                                    })
                                    .collect();
                                if !related.is_empty() {
                                    ui.add_space(8.0);
                                    ui.label(RichText::new("LINKS").size(11.0).color(DIM));
                                    for r in related {
                                        ui.label(RichText::new(r).size(12.0).color(TEXT));
                                    }
                                }
                            });
                    });
            });
        if close {
            view.element = None;
        }
    }
    if let Some(place) = opened {
        open_at(infra, &place, app);
    }
}

fn clip(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        text.to_owned()
    } else {
        format!("{}…", text.chars().take(most - 1).collect::<String>())
    }
}

/// What is known of an element and where it is configured; returns the
/// place clicked.
pub(crate) fn element_details(ui: &mut Ui, element: &Element) -> Option<Place> {
    let mut open = None;
    ui.label(RichText::new(&element.name).size(18.0).color(TEXT));
    ui.label(
        RichText::new(format!("{} · {}", element.kind.label(), element.summary))
            .size(12.0)
            .color(colour(element.kind)),
    );
    if let Some(ns) = &element.namespace {
        ui.label(
            RichText::new(format!("namespace {ns}"))
                .size(12.0)
                .color(DIM),
        );
    }
    if !element.properties.is_empty() {
        ui.add_space(6.0);
        egui::Grid::new(("element-properties", &element.name))
            .num_columns(2)
            .spacing(vec2(12.0, 3.0))
            .show(ui, |ui| {
                for (key, value) in &element.properties {
                    ui.label(RichText::new(key).size(11.5).color(DIM));
                    ui.add(
                        egui::Label::new(RichText::new(value).monospace().size(11.5).color(TEXT))
                            .wrap(),
                    );
                    ui.end_row();
                }
            });
    }
    if !element.places.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new("CONFIGURED IN").size(11.0).color(DIM));
        for place in &element.places {
            let shown = ui.add(
                egui::Label::new(
                    RichText::new(place.to_string())
                        .monospace()
                        .size(11.0)
                        .color(LINK),
                )
                .sense(Sense::click()),
            );
            if shown
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                open = Some(place.clone());
            }
        }
    }
    open
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironquill_codemap::ArchLink;

    fn element(kind: ElementKind, name: &str) -> Element {
        Element {
            kind,
            name: name.into(),
            summary: String::new(),
            properties: Vec::new(),
            places: Vec::new(),
            namespace: None,
            service: None,
            replicas: None,
        }
    }

    #[test]
    fn the_way_in_left_workloads_middle_cloud_right() {
        let deployment = Deployment {
            environment: "prod".into(),
            elements: vec![
                element(ElementKind::Database, "db"),
                element(ElementKind::Workload, "api"),
                element(ElementKind::LoadBalancer, "alb"),
                element(ElementKind::Cluster, "cluster"),
            ],
            links: vec![ArchLink {
                from: 2,
                to: 1,
                kind: LinkKind::Routes,
                label: String::new(),
            }],
            notes: Vec::new(),
        };
        let room = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 600.0));
        let cards = arrange(&deployment, room);
        let x = |i: usize| cards[i].unwrap().center().x;
        assert!(x(2) < x(1));
        assert_eq!(x(0), x(1), "the database under the workloads");
        assert!(cards[0].unwrap().top() > cards[1].unwrap().top());
        assert!(cards[3].is_none(), "the cluster is a frame, not a card");
    }
}
