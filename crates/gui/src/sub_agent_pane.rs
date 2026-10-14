//! A task handed to a model of the team, at work (Ctrl-T), under the
//! conversation: who it is, the task, then everything it does, the latest
//! at the bottom unless scrolled back.

use bevy_egui::egui::{self, Align, CornerRadius, Frame, Layout, Margin, RichText, Stroke, Ui};
use universe_ui::keymap::Focus;
use universe_ui::{App, Effect};

use crate::theme::{DIM, MAGENTA, TEXT};
use crate::transcript;

/// Draws the pane, and says whether it was clicked. `seen` is how far back
/// the keys had scrolled it last frame, in rows, so that only their moves
/// since are applied: the wheel scrolls it as any list.
pub(crate) fn show(ui: &mut Ui, app: &App, seen: &mut usize, effects: &mut Vec<Effect>) -> bool {
    let Some(sub) = app.sub_agent() else {
        return false;
    };
    let focused = app.focus() == Focus::SubAgent;
    let border = if focused {
        MAGENTA
    } else {
        MAGENTA.gamma_multiply(0.45)
    };
    let shown = Frame::new()
        .stroke(Stroke::new(1.0, border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Sub-agent").strong().color(MAGENTA));
                ui.label(RichText::new(sub.to.to_string()).strong().color(TEXT));
                ui.label(RichText::new(format!("for {}", sub.from)).color(DIM));
                let state = if sub.working {
                    format!(
                        "working {}",
                        crate::SPINNER[app.spinner() % crate::SPINNER.len()]
                    )
                } else {
                    format!(
                        "done · {}",
                        transcript::plural(sub.work.len(), "step", "steps")
                    )
                };
                ui.label(RichText::new(state).color(MAGENTA));
                ui.label(RichText::new(sub.spent.to_string()).small().color(DIM));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new("Ctrl-T hides").small().color(DIM));
                });
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Task").color(MAGENTA));
                ui.label(RichText::new(sub.task).color(DIM));
            });
            ui.separator();
            let row = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
            let back = app.sub_scroll();
            let delta = back as f32 - *seen as f32;
            *seen = back;
            let output = egui::ScrollArea::vertical()
                .id_salt("sub-agent")
                .stick_to_bottom(true)
                .auto_shrink(false)
                .show(ui, |ui| {
                    if delta != 0.0 {
                        ui.scroll_with_delta(egui::vec2(0.0, delta * row));
                    }
                    for entry in &sub.work {
                        transcript::entry_ui(ui, app, entry, effects);
                    }
                    if sub.work.is_empty() {
                        ui.label(RichText::new("starting…").color(DIM));
                    }
                });
            let rows = (output.content_size.y - output.inner_rect.height()).max(0.0) / row;
            app.set_sub_max(rows as usize);
        });
    // Read without taking the click from a link or a button under it.
    ui.rect_contains_pointer(shown.response.rect) && ui.input(|i| i.pointer.primary_clicked())
}
