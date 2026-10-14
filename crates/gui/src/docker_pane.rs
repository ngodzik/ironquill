//! The running containers (Ctrl-K), along the bottom: as `docker ps` said
//! last, healthy ones green, the others amber, the chosen one lit while the
//! pane has the focus.

use bevy_egui::egui::{self, Align, Grid, Layout, RichText, Sense, Ui};
use universe_ui::App;
use universe_ui::input::{KeyCode, KeyEvent, KeyModifiers};
use universe_ui::keymap::Focus;

use crate::theme::{ACCENT, DIM, GREEN, RED, SELECTED, YELLOW};

/// The widest a column other than the ports may be, in characters.
const COLUMN: usize = 32;

/// What a click in the pane asks for.
pub(crate) enum Clicked {
    /// The container at this place in the list.
    Container(usize),
    /// The pane, outside any container.
    Pane,
}

/// Draws the pane. A key its own button stands for goes to `keys`.
pub(crate) fn show(ui: &mut Ui, app: &App, keys: &mut Vec<KeyEvent>) -> Option<Clicked> {
    let pane = app.docker()?;
    let focused = app.focus() == Focus::Docker;
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Docker")
                .strong()
                .color(if focused { ACCENT } else { DIM }),
        );
        if pane.loaded && pane.error.is_none() {
            ui.label(RichText::new(format!("{} running", pane.containers.len())).color(DIM));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .small_button("hide")
                .on_hover_text("Ctrl-K shows or hides the containers")
                .clicked()
            {
                keys.push(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
            }
        });
    });
    if let Some(error) = &pane.error {
        ui.label(RichText::new(error).color(RED));
        return None;
    }
    if !pane.loaded {
        ui.label(RichText::new("Asking docker…").color(DIM));
        return None;
    }
    if pane.containers.is_empty() {
        ui.label(RichText::new("No container running").color(DIM));
        return None;
    }
    let cut = |text: &str| -> String {
        if text.chars().count() > COLUMN {
            text.chars().take(COLUMN - 1).chain(['…']).collect()
        } else {
            text.to_owned()
        }
    };
    let chosen = focused.then_some(pane.selected);
    egui::ScrollArea::vertical()
        .id_salt("docker")
        .auto_shrink(false)
        .show(ui, |ui| {
            Grid::new("containers")
                .num_columns(4)
                .spacing([18.0, 3.0])
                // The header is the grid's first row.
                .with_row_color(move |row, _| (chosen == row.checked_sub(1)).then_some(SELECTED))
                .show(ui, |ui| {
                    for header in ["NAME", "IMAGE", "STATUS", "PORTS"] {
                        ui.label(RichText::new(header).monospace().small().color(DIM));
                    }
                    ui.end_row();
                    for (i, c) in pane.containers.iter().enumerate() {
                        let colour = if healthy(&c.status) { GREEN } else { YELLOW };
                        let cells = [cut(&c.name), cut(&c.image), cut(&c.status), c.ports.clone()];
                        let mut row = None::<egui::Response>;
                        for text in cells {
                            let label = ui.add(
                                egui::Label::new(RichText::new(text).monospace().color(colour))
                                    .selectable(false)
                                    .sense(Sense::click()),
                            );
                            row = Some(match row {
                                Some(row) => row.union(label),
                                None => label,
                            });
                        }
                        ui.end_row();
                        if let Some(row) = row {
                            if row.clicked() {
                                clicked = Some(Clicked::Container(i));
                            }
                            if chosen == Some(i) {
                                row.scroll_to_me(None);
                            }
                        }
                    }
                });
        });
    // A click anywhere else in the pane, read without taking it from what
    // is under it.
    if clicked.is_none()
        && ui.rect_contains_pointer(ui.min_rect())
        && ui.input(|i| i.pointer.primary_clicked())
    {
        clicked = Some(Clicked::Pane);
    }
    clicked
}

/// How tall the pane wants to be for `containers`, in points: a row each
/// under its title and header, never more than a third of `room`.
pub(crate) fn height(ui: &Ui, containers: usize, room: f32) -> f32 {
    let row = ui.text_style_height(&egui::TextStyle::Body) + 3.0;
    let wanted = row * (containers.max(1) as f32 + 2.0) + 28.0;
    wanted.clamp(row * 3.0 + 28.0, (room / 3.0).max(row * 3.0 + 28.0))
}

/// Whether a container whose status reads `status` is running well.
fn healthy(status: &str) -> bool {
    status.starts_with("Up") && !status.contains("unhealthy")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_and_not_unhealthy_reads_as_healthy() {
        assert!(healthy("Up 2 hours"));
        assert!(healthy("Up 5 minutes (healthy)"));
        assert!(!healthy("Up 1 minute (unhealthy)"));
        assert!(!healthy("Exited (1) 3 seconds ago"));
        assert!(!healthy("Restarting (1) 2 seconds ago"));
    }
}
