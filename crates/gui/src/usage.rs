//! The usage pane (Ctrl-O): where the money goes, how full the conversation
//! is, and when a cache was rebuilt, drawn to be read at a glance and
//! explored with the mouse: hover for a call, drag to pan, scroll to zoom,
//! double-click to come back.

use std::time::Instant;

use bevy_egui::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke, Ui, vec2,
};
use egui_plot::{
    Corner, GridMark, HoverPosition, Legend, Line, MarkerShape, Plot, PlotPoints, Points,
    uniform_grid_spacer,
};
use ironquill_core::TokenCount;
use ironquill_ui::sessions;
use ironquill_ui::usage::{ModelUse, UsageLog, window_name};

use crate::theme::{ACCENT, CYAN, DIM, EDGE, RAISED, TEXT, rgb};

/// How long the pane takes to draw itself in, in seconds.
const REVEAL_SECONDS: f32 = 0.9;

/// The windows Ctrl-P goes through, as the state does.
const WINDOWS: [u64; 3] = [3600, 6 * 3600, 24 * 3600];

/// Where cache rebuilds stand out: the warm orange of the terminal's marks.
const REBUILD: Color32 = Color32::from_rgb(230, 126, 34);

/// The context line, neutral beside the models' colours.
const CONTEXT: Color32 = Color32::from_rgb(150, 160, 180);

/// What the pane remembers between frames: when it opened, to draw itself
/// in.
#[derive(Debug, Default)]
pub(crate) struct UsageView {
    opened: Option<Instant>,
}

impl UsageView {
    /// How far the pane has drawn itself in, from 0 to 1, eased; asks for
    /// the next frame while it is not done.
    fn progress(&mut self, ctx: &egui::Context) -> f32 {
        let opened = *self.opened.get_or_insert_with(Instant::now);
        let t = (opened.elapsed().as_secs_f32() / REVEAL_SECONDS).min(1.0);
        if t < 1.0 {
            ctx.request_repaint();
        }
        // Fast, then settling: an ease-out cubic.
        1.0 - (1.0 - t).powi(3)
    }

    /// The pane closed: it draws itself in again when next opened.
    pub(crate) fn closed(&mut self) {
        self.opened = None;
    }
}

/// Draws the pane for the last `window` seconds of `log`.
pub(crate) fn show(ui: &mut Ui, view: &mut UsageView, log: &UsageLog, window: u64) {
    let progress = view.progress(ui.ctx());
    let now = sessions::now();
    let from = now.saturating_sub(window);
    let models = log.models(from);

    header(ui, window);
    ui.add_space(10.0);
    if models.is_empty() {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No call in this window yet").color(DIM));
        });
        return;
    }

    tiles(ui, log, &models, from, window, progress);
    ui.add_space(14.0);
    section(ui, "Cost", "added up, per model");
    cost_plot(ui, log, &models, from, window, progress);
    ui.add_space(14.0);
    section(
        ui,
        "Context",
        "how full the conversation is, and the caches rebuilt",
    );
    context_plot(ui, log, from, window, progress);
    ui.add_space(14.0);
    section(
        ui,
        "Models",
        "what each cost, and how much it read from the cache",
    );
    breakdown(ui, &models, progress);
}

/// The title and the windows, the one shown lit.
fn header(ui: &mut Ui, window: u64) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Usage").size(20.0).strong().color(TEXT));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new("Ctrl-P").small().color(DIM));
            for w in WINDOWS.iter().rev() {
                let on = *w == window;
                let chip = RichText::new(window_name(*w)).small();
                Frame::new()
                    .fill(if on {
                        ACCENT.gamma_multiply(0.25)
                    } else {
                        RAISED
                    })
                    .stroke(Stroke::new(1.0, if on { ACCENT } else { EDGE }))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::symmetric(8, 2))
                    .show(ui, |ui| {
                        ui.label(if on {
                            chip.color(ACCENT).strong()
                        } else {
                            chip.color(DIM)
                        });
                    });
            }
            if !WINDOWS.contains(&window) {
                ui.label(RichText::new(window_name(window)).small().color(ACCENT));
            }
        });
    });
}

fn section(ui: &mut Ui, title: &str, what: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong().color(TEXT));
        ui.label(RichText::new(what).small().color(DIM));
    });
}

/// The four numbers that matter, counting up as the pane opens.
fn tiles(ui: &mut Ui, log: &UsageLog, models: &[ModelUse], from: u64, window: u64, progress: f32) {
    let cost: f64 = models.iter().map(|m| m.cost).sum();
    let calls: usize = models.iter().map(|m| m.calls).sum();
    let rebuilds: usize = models.iter().map(|m| m.rebuilds).sum();
    let per_hour = cost / (window as f64 / 3600.0);
    let shown = f64::from(progress);
    let cache = log.cache_share(from);

    let tiles = [
        (format!("${:.3}", cost * shown), "spent".to_owned(), ACCENT),
        (
            format!("{}", (calls as f64 * shown).round() as usize),
            if rebuilds == 0 {
                "calls".to_owned()
            } else {
                format!("calls · {rebuilds} rebuilt")
            },
            TEXT,
        ),
        (
            cache.map_or_else(|| "?".to_owned(), |c| format!("{:.0}%", c * 100.0 * shown)),
            "cached".to_owned(),
            CYAN,
        ),
        (
            format!("${:.2}", per_hour * shown),
            "per hour".to_owned(),
            TEXT,
        ),
    ];
    let gap = 8.0;
    // Each tile's margin and edge come on top of the width it is given.
    let width = ((ui.available_width() - gap * 3.0) / 4.0 - 22.0).max(40.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (value, label, colour) in tiles {
            Frame::new()
                .fill(RAISED)
                .stroke(Stroke::new(1.0, EDGE))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(value).size(19.0).strong().color(colour));
                        ui.add(
                            egui::Label::new(RichText::new(label).small().color(DIM)).truncate(),
                        );
                    });
                });
        }
    });
}

/// Seconds after `from` as minutes before now, the plots' x.
fn minutes_before_now(seconds_after_from: f64, window: u64) -> f64 {
    (seconds_after_from - window as f64) / 60.0
}

/// Minutes before now as a person reads them: `-35m`, `-2h`, `now`.
fn ago(minutes: f64) -> String {
    let m = -minutes;
    if m < 0.5 {
        "now".to_owned()
    } else if m < 90.0 {
        format!("-{m:.0}m")
    } else {
        format!("-{:.1}h", m / 60.0)
    }
}

/// A round step that cuts `top` into about `marks` parts: 1, 2 or 5 times a
/// power of ten.
fn nice_step(top: f64, marks: f64) -> f64 {
    let raw = (top / marks).max(f64::MIN_POSITIVE);
    let magnitude = 10_f64.powf(raw.log10().floor());
    let scaled = raw / magnitude;
    let nice = if scaled <= 1.0 {
        1.0
    } else if scaled <= 2.0 {
        2.0
    } else if scaled <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * magnitude
}

/// Tokens on an axis, short: `40k`, `1.2M`.
fn tokens(n: f64) -> String {
    if n >= 1_000_000.0 {
        format!("{:.1}M", n / 1_000_000.0)
    } else if n >= 1_000.0 {
        format!("{:.0}k", n / 1_000.0)
    } else {
        format!("{n:.0}")
    }
}

/// The points up to how far the pane has drawn itself in, the last one cut
/// where the reveal stands, so that each line grows from the left.
fn revealed(points: &[(f64, f64)], window: u64, progress: f32) -> Vec<[f64; 2]> {
    let edge = -(window as f64 / 60.0) * (1.0 - f64::from(progress));
    let mut out: Vec<[f64; 2]> = Vec::new();
    for &(x, y) in points {
        let x = minutes_before_now(x, window);
        if x > edge {
            if let Some(&[_, last_y]) = out.last() {
                out.push([edge, last_y]);
            }
            break;
        }
        out.push([x, y]);
    }
    out
}

/// A plot over the window, up to `top`: the frame is the final one from
/// the first frame, so that the lines grow into it rather than the axes
/// shifting under them.
fn base_plot(id: &str, window: u64, top: f64) -> Plot<'static> {
    let span = window as f64 / 60.0;
    // Marks every ten minutes over an hour, every hour over six, every four
    // over a day: a few labels that read at once.
    let step = (span / 6.0).max(10.0);
    let level = nice_step(top, 3.0);
    Plot::new(id.to_owned())
        .height(170.0)
        .show_background(false)
        .include_x(-span)
        .include_x(0.0)
        .include_y(0.0)
        .include_y(top)
        .allow_boxed_zoom(false)
        .x_grid_spacer(uniform_grid_spacer(move |_| [step / 2.0, step, step * 3.0]))
        .y_grid_spacer(uniform_grid_spacer(move |_| {
            [level / 2.0, level, level * 4.0]
        }))
        .x_axis_formatter(|mark: GridMark, _| ago(mark.value))
        .show_axes([true, true])
}

/// Each model's cost added up over the window: a glowing line that steps up
/// at each call, filled down to zero.
fn cost_plot(
    ui: &mut Ui,
    log: &UsageLog,
    models: &[ModelUse],
    from: u64,
    window: u64,
    progress: f32,
) {
    let lines: Vec<(String, Color32, Vec<[f64; 2]>)> = models
        .iter()
        .map(|m| {
            let mut steps = log.cost_steps(&m.model, from);
            let last = steps.last().map_or(0.0, |p| p.1);
            steps.push((window as f64, last));
            (
                m.model.clone(),
                rgb(m.color),
                revealed(&steps, window, progress),
            )
        })
        .collect();
    let top = models
        .iter()
        .map(|m| m.cost)
        .fold(0.0_f64, f64::max)
        .max(0.001)
        * 1.15;
    base_plot("usage-cost", window, top)
        .legend(
            Legend::default()
                .position(Corner::LeftTop)
                .background_alpha(0.85),
        )
        .y_axis_formatter(|mark: GridMark, _| format!("${:.2}", mark.value))
        .label_formatter(|hover: &HoverPosition<'_>| match hover {
            HoverPosition::NearDataPoint {
                plot_name,
                position,
                ..
            } if !plot_name.is_empty() => Some(format!(
                "{plot_name}\n${:.4} by {}",
                position.y,
                ago(position.x)
            )),
            _ => None,
        })
        .show(ui, |plot| {
            for (name, colour, points) in lines {
                // Two wide faint lines under the sharp one: the glow.
                for (width, alpha) in [(12.0, 0.07), (6.0, 0.18)] {
                    plot.line(
                        Line::new("", PlotPoints::from(points.clone()))
                            .color(colour.gamma_multiply(alpha))
                            .width(width)
                            .allow_hover(false),
                    );
                }
                let head = points.last().copied();
                plot.line(
                    Line::new(name, PlotPoints::from(points))
                        .color(colour)
                        .width(2.0)
                        .fill(0.0)
                        .fill_alpha(0.14),
                );
                // Where the line stands now, lit.
                if let Some(head) = head {
                    head_mark(plot, head, colour);
                }
            }
        });
}

/// A point lit at the head of a line: a halo around a bright dot.
fn head_mark(plot: &mut egui_plot::PlotUi<'_>, at: [f64; 2], colour: Color32) {
    for (radius, alpha) in [(10.0, 0.12), (6.0, 0.3)] {
        plot.points(
            Points::new("", PlotPoints::from(vec![at]))
                .color(colour.gamma_multiply(alpha))
                .radius(radius)
                .allow_hover(false),
        );
    }
    plot.points(
        Points::new("", PlotPoints::from(vec![at]))
            .color(colour)
            .radius(3.5)
            .allow_hover(false),
    );
}

/// The conversation's context over time, and the calls that wrote most of
/// their input to the cache: it had expired.
fn context_plot(ui: &mut Ui, log: &UsageLog, from: u64, window: u64, progress: f32) {
    let line = log.context_line(from);
    let top = line
        .iter()
        .chain(&log.rebuilds(from))
        .map(|p| p.1)
        .fold(1_000.0_f64, f64::max)
        * 1.15;
    let context = revealed(&line, window, progress);
    let rebuilds: Vec<[f64; 2]> = revealed(&log.rebuilds(from), window, progress);
    base_plot("usage-context", window, top)
        .height(130.0)
        .y_axis_formatter(|mark: GridMark, _| tokens(mark.value.max(0.0)))
        .label_formatter(|hover: &HoverPosition<'_>| match hover {
            HoverPosition::NearDataPoint {
                plot_name: "cache rebuilt",
                position,
                ..
            } => Some(format!(
                "cache rebuilt {}\n{} written again",
                ago(position.x),
                TokenCount(position.y as u64)
            )),
            HoverPosition::NearDataPoint {
                plot_name: "context",
                position,
                ..
            } => Some(format!(
                "context {}\n{}",
                ago(position.x),
                TokenCount(position.y as u64)
            )),
            _ => None,
        })
        .legend(
            Legend::default()
                .position(Corner::LeftTop)
                .background_alpha(0.85),
        )
        .show(ui, |plot| {
            let head = context.last().copied();
            plot.line(
                Line::new("", PlotPoints::from(context.clone()))
                    .color(CONTEXT.gamma_multiply(0.15))
                    .width(6.0)
                    .allow_hover(false),
            );
            plot.line(
                Line::new("context", PlotPoints::from(context))
                    .color(CONTEXT)
                    .width(1.5)
                    .fill(0.0)
                    .fill_alpha(0.08),
            );
            if let Some(head) = head {
                head_mark(plot, head, CONTEXT);
            }
            plot.points(
                Points::new("", PlotPoints::from(rebuilds.clone()))
                    .color(REBUILD.gamma_multiply(0.25))
                    .radius(9.0)
                    .allow_hover(false),
            );
            plot.points(
                Points::new("cache rebuilt", PlotPoints::from(rebuilds))
                    .color(REBUILD)
                    .shape(MarkerShape::Diamond)
                    .filled(true)
                    .radius(5.0),
            );
        });
}

/// One bar per model, as long as its share of the cost, growing as the
/// pane opens, with what it cost and how well it used the cache.
fn breakdown(ui: &mut Ui, models: &[ModelUse], progress: f32) {
    let top = models
        .iter()
        .map(|m| m.cost)
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    let total: f64 = models.iter().map(|m| m.cost).sum::<f64>().max(1e-9);
    for model in models {
        let colour = rgb(model.color);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("●").color(colour));
            ui.label(RichText::new(&model.model).color(TEXT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("${:.3}", model.cost))
                        .strong()
                        .color(colour),
                );
                ui.label(
                    RichText::new(format!("{:.0}%", model.cost / total * 100.0))
                        .small()
                        .color(DIM),
                );
            });
        });
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, RAISED);
        let share = (model.cost / top) as f32 * progress;
        let mut bar = rect;
        bar.set_width((rect.width() * share).max(2.0));
        painter.rect_filled(bar.expand(2.0), 5.0, colour.gamma_multiply(0.15));
        painter.rect_filled(bar, 3.0, colour);
        let cache = model.cache_share.map_or_else(
            || "cache ?".to_owned(),
            |s| format!("cache {:.0}%", s * 100.0),
        );
        let rebuilt = if model.rebuilds == 0 {
            String::new()
        } else {
            format!(" · {} rebuilt", model.rebuilds)
        };
        ui.label(
            RichText::new(format!(
                "{} call{} · {cache}{rebuilt}",
                model.calls,
                if model.calls == 1 { "" } else { "s" }
            ))
            .small()
            .color(DIM),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_reads_as_minutes_or_hours_before_now() {
        assert_eq!(ago(0.0), "now");
        assert_eq!(ago(-35.0), "-35m");
        assert_eq!(ago(-150.0), "-2.5h");
        assert!((minutes_before_now(3000.0, 3600) - -10.0).abs() < 1e-9);
    }

    #[test]
    fn axis_steps_are_round() {
        assert!((nice_step(0.84, 3.0) - 0.5).abs() < 1e-12);
        assert!((nice_step(130_000.0, 3.0) - 50_000.0).abs() < 1e-6);
        assert!((nice_step(0.012, 3.0) - 0.005).abs() < 1e-12);
        assert_eq!(tokens(50_000.0), "50k");
        assert_eq!(tokens(1_250_000.0), "1.2M");
        assert_eq!(tokens(0.0), "0");
    }

    #[test]
    fn a_line_grows_from_the_left_as_the_pane_opens() {
        let steps = [(0.0, 0.0), (1800.0, 0.0), (1800.0, 1.0), (3600.0, 1.0)];
        // Fully open: every point.
        assert_eq!(revealed(&steps, 3600, 1.0).len(), 4);
        // Half open: up to the middle, the last point cut at the edge.
        let half = revealed(&steps, 3600, 0.5);
        assert_eq!(half.last(), Some(&[-30.0, 1.0]));
        assert!(half.iter().all(|p| p[0] <= -30.0));
        // Closed: nothing past the start.
        assert!(revealed(&steps, 3600, 0.0).iter().all(|p| p[0] <= -60.0));
    }
}
