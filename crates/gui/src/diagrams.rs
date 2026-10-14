//! Mermaid diagrams in the window: each drawn once on a thread, by the
//! renderer the terminal uses, into the same cache; the PNG decoded and
//! shown as a texture. Its code shows while it draws, and stays if it fails.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bevy_egui::egui::{self, ColorImage, TextureHandle, TextureOptions, Ui};
use universe_ui::diagrams::Renderer;

/// Where a diagram is.
enum State {
    Drawing,
    Drawn(TextureHandle),
    Failed(String),
}

/// A diagram drawn on a thread: its key, and its pixels or why it failed.
type Drawn = (u64, Result<ColorImage, String>);

/// The diagrams of the conversation, shared with the threads that draw them.
#[derive(Clone)]
pub(crate) struct Diagrams {
    renderer: Option<Renderer>,
    cache: Option<PathBuf>,
    states: Arc<Mutex<HashMap<u64, State>>>,
    /// Images drawn, waiting to become textures on the main thread.
    drawn: Arc<Mutex<Vec<Drawn>>>,
}

impl Diagrams {
    /// Diagrams drawn by the renderer `configured` in the settings, else the
    /// first installed.
    pub(crate) fn new(configured: Option<&[String]>) -> Self {
        Self {
            renderer: Renderer::find(configured, Renderer::installed),
            cache: universe_ui::diagrams::cache(),
            states: Arc::default(),
            drawn: Arc::default(),
        }
    }

    /// Draws the diagram of `source` in `ui`, once drawn; `false` when it is
    /// not, and its code should show: being drawn, failed, or no renderer.
    pub(crate) fn show(&self, ui: &mut Ui, source: &str) -> bool {
        let (Some(renderer), Some(cache)) = (&self.renderer, &self.cache) else {
            return false;
        };
        self.take_drawn(ui.ctx());
        let key = renderer.key(source);
        let Ok(mut states) = self.states.lock() else {
            return false;
        };
        match states.get(&key) {
            Some(State::Drawn(texture)) => {
                let size = texture.size_vec2();
                let width = size.x.min(ui.available_width());
                let shown = egui::vec2(width, size.y * width / size.x.max(1.0));
                ui.add(egui::Image::new((texture.id(), shown)));
                true
            }
            Some(State::Failed(why)) => {
                ui.label(
                    egui::RichText::new(format!("The diagram could not be drawn: {why}"))
                        .small()
                        .color(crate::theme::DIM),
                );
                false
            }
            Some(State::Drawing) => {
                ui.label(
                    egui::RichText::new("◌ drawing the diagram…")
                        .small()
                        .color(crate::theme::DIM),
                );
                false
            }
            None => {
                states.insert(key, State::Drawing);
                let (renderer, cache, source) =
                    (renderer.clone(), cache.clone(), source.to_owned());
                let (drawn, ctx) = (Arc::clone(&self.drawn), ui.ctx().clone());
                std::thread::spawn(move || {
                    let done = cache.join(format!("{key:016x}.png"));
                    let path = if done.is_file() {
                        Ok(done)
                    } else {
                        renderer.draw(&source, &cache)
                    };
                    let image = path.and_then(|p| decode(&p));
                    if let Ok(mut drawn) = drawn.lock() {
                        drawn.push((key, image));
                    }
                    ctx.request_repaint();
                });
                false
            }
        }
    }

    /// Turns the images drawn into textures.
    fn take_drawn(&self, ctx: &egui::Context) {
        let ready: Vec<_> = self
            .drawn
            .lock()
            .map(|mut d| std::mem::take(&mut *d))
            .unwrap_or_default();
        let Ok(mut states) = self.states.lock() else {
            return;
        };
        for (key, image) in ready {
            let state = match image {
                Ok(image) => State::Drawn(ctx.load_texture(
                    format!("diagram-{key:016x}"),
                    image,
                    TextureOptions::LINEAR,
                )),
                Err(why) => State::Failed(why),
            };
            states.insert(key, state);
        }
    }
}

/// The PNG at `path` as pixels.
fn decode(path: &std::path::Path) -> Result<ColorImage, String> {
    let image = image::open(path).map_err(|e| e.to_string())?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, image.as_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drawn_png_is_read_as_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.png");
        image::RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 255]))
            .save(&path)
            .unwrap();
        let pixels = decode(&path).unwrap();
        assert_eq!(pixels.size, [3, 2]);
        assert!(decode(&dir.path().join("none.png")).is_err());
    }
}
