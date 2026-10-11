//! The quiet pattern behind the panels, as `/background` chose it.
//!
//! Laid out over the whole window, so that it runs on from one panel to
//! the next, and painted at the start of each main panel, over its colour
//! and under its content. Its alpha follows the window's opacity, as the
//! panels' fill does: see-through, it fades with them rather than stand out
//! over the desktop, and it never paints a fill of its own.

use std::f32::consts::TAU;

use bevy_egui::egui::{self, Color32, Pos2, Rect, Shape, Stroke, Ui, pos2, vec2};
use universe_ui::App;
use universe_ui::backdrop::{Backdrop, Motion, Pattern};

/// The pattern's colour, a cold grey-blue that reads on the dark panels.
const TINT: [u8; 3] = [150, 170, 215];

/// Paints the chosen pattern behind what `ui` is about to draw, clipped to
/// its panel. Nothing without a pattern, or beside a map.
pub(crate) fn paint(ui: &Ui, app: &App) {
    let backdrop = app.backdrop();
    if backdrop.pattern.is_none() || app.map_view().is_some() {
        return;
    }
    let ctx = ui.ctx();
    // The same fade as the panels' fill, read from the same animation.
    let opacity = ctx.animate_value_with_time(egui::Id::new("opacity"), app.opacity(), 0.25);
    let time = ctx.input(|i| i.time);
    let shapes = shapes(backdrop, ctx.content_rect(), time, opacity);
    ui.painter().with_clip_rect(ui.clip_rect()).extend(shapes);
}

/// The shapes of `backdrop` over `area` at `time`, in seconds; a still
/// pattern is the same at any time. A few hundred at most.
pub(crate) fn shapes(backdrop: Backdrop, area: Rect, time: f64, opacity: f32) -> Vec<Shape> {
    let Some(pattern) = backdrop.pattern else {
        return Vec::new();
    };
    let t = match backdrop.motion {
        Motion::Still => 0.0,
        // Wrapped, so that the angles stay precise after hours.
        Motion::Slow => (time % 3600.0) as f32,
    };
    let colour = |alpha: f32| {
        let [r, g, b] = TINT;
        Color32::from_rgba_unmultiplied(r, g, b, (alpha * opacity.clamp(0.0, 1.0)).round() as u8)
    };
    match pattern {
        Pattern::Stars => stars(area, t, colour),
        Pattern::Waves => waves(area, t, colour),
        Pattern::Grid => grid(area, t, colour),
        Pattern::Rain => rain(area, t, colour),
    }
}

/// A number from 0 to 1 that looks random but is always the same for `i`
/// and `salt`: the pattern does not jump from one frame to the next.
fn noise(i: u32, salt: u32) -> f32 {
    let mut x = u64::from(i) << 32 | u64::from(salt);
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

/// How many of a pattern's pieces fit `area`, one per `room` square
/// points, between `least` and `most`.
fn count(area: Rect, room: f32, least: u32, most: u32) -> u32 {
    ((area.area() / room) as u32).clamp(least, most)
}

fn stars(area: Rect, t: f32, colour: impl Fn(f32) -> Color32) -> Vec<Shape> {
    (0..count(area, 9_000.0, 60, 400))
        .map(|i| {
            let at = pos2(
                area.left() + noise(i, 1) * area.width(),
                area.top() + noise(i, 2) * area.height(),
            );
            let radius = 0.6 + noise(i, 3);
            // A slow twinkle, each star at its own pace.
            let twinkle = 0.65 + 0.35 * (t * (0.4 + noise(i, 4)) + noise(i, 5) * TAU).sin();
            Shape::circle_filled(at, radius, colour((25.0 + 45.0 * noise(i, 6)) * twinkle))
        })
        .collect()
}

fn waves(area: Rect, t: f32, colour: impl Fn(f32) -> Color32) -> Vec<Shape> {
    const LINES: u32 = 7;
    const POINTS: u32 = 80;
    (0..LINES)
        .map(|k| {
            let middle = area.top() + (k as f32 + 0.5) / LINES as f32 * area.height();
            let amplitude = 14.0 + 22.0 * noise(k, 1);
            let length = 380.0 + 320.0 * noise(k, 2);
            let phase = noise(k, 3) * TAU + t * (0.15 + 0.1 * noise(k, 4));
            let points: Vec<Pos2> = (0..=POINTS)
                .map(|p| {
                    let x = area.left() + p as f32 / POINTS as f32 * area.width();
                    pos2(x, middle + amplitude * (x / length * TAU + phase).sin())
                })
                .collect();
            Shape::line(points, Stroke::new(1.2, colour(22.0)))
        })
        .collect()
}

fn grid(area: Rect, t: f32, colour: impl Fn(f32) -> Color32) -> Vec<Shape> {
    const SPACING: f32 = 56.0;
    let shift = (t * 3.0) % SPACING;
    let stroke = Stroke::new(1.0, colour(12.0));
    let mut shapes = Vec::new();
    let mut x = area.left() + shift;
    while x < area.right() {
        shapes.push(Shape::line_segment(
            [pos2(x, area.top()), pos2(x, area.bottom())],
            stroke,
        ));
        x += SPACING;
    }
    let mut y = area.top() + shift;
    while y < area.bottom() {
        shapes.push(Shape::line_segment(
            [pos2(area.left(), y), pos2(area.right(), y)],
            stroke,
        ));
        y += SPACING;
    }
    shapes
}

fn rain(area: Rect, t: f32, colour: impl Fn(f32) -> Color32) -> Vec<Shape> {
    (0..count(area, 14_000.0, 40, 300))
        .map(|i| {
            let length = 10.0 + 18.0 * noise(i, 1);
            let speed = 30.0 + 40.0 * noise(i, 2);
            let fall = area.height() + length;
            let y = area.top() - length + (noise(i, 3) * fall + t * speed) % fall;
            let x = area.left() + noise(i, 4) * area.width();
            // A little slanted, as rain falls in a light wind.
            let top = pos2(x, y);
            Shape::line_segment(
                [top, top + vec2(-0.15 * length, length)],
                Stroke::new(1.0, colour(18.0 + 26.0 * noise(i, 5))),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_pattern_stands_still_unless_it_moves_and_stays_light() {
        // A large screen, the most shapes.
        let area = Rect::from_min_size(Pos2::ZERO, vec2(3840.0, 2160.0));
        for pattern in Pattern::ALL {
            let still = Backdrop {
                pattern: Some(pattern),
                motion: Motion::Still,
            };
            let first = shapes(still, area, 1.0, 1.0);
            assert_eq!(first, shapes(still, area, 2.5, 1.0), "{pattern:?} moved");
            assert!(!first.is_empty() && first.len() < 2_000, "{pattern:?}");
            let slow = Backdrop {
                motion: Motion::Slow,
                ..still
            };
            assert_ne!(
                shapes(slow, area, 1.0, 1.0),
                shapes(slow, area, 2.5, 1.0),
                "{pattern:?} did not move"
            );
        }
        assert!(shapes(Backdrop::default(), area, 1.0, 1.0).is_empty());
    }

    #[test]
    fn the_pattern_fades_with_the_window() {
        let area = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        let backdrop = Backdrop {
            pattern: Some(Pattern::Grid),
            motion: Motion::Still,
        };
        let alpha = |opacity: f32| match &shapes(backdrop, area, 0.0, opacity)[0] {
            Shape::LineSegment { stroke, .. } => stroke.color.a(),
            other => panic!("a grid is lines, not {other:?}"),
        };
        assert!(alpha(1.0) > 0);
        assert!(alpha(0.3) < alpha(1.0));
        assert_eq!(alpha(0.0), 0);
    }
}
