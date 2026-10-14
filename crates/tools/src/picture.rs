//! Images a model is shown with `view_image`: read from the workspace, told
//! apart by their first bytes, and halved when a provider refuses one as
//! too large, since none publishes its limit.

use std::io::Cursor;

use base64::Engine;

use crate::error::ToolError;

/// The largest image a model is shown.
pub const MOST_BYTES: usize = 20 * 1024 * 1024;

/// An image, as sent to a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    /// Its media type: `image/png`, `image/jpeg`, `image/gif`, `image/webp`.
    pub mime: String,
    /// What the file holds.
    pub bytes: Vec<u8>,
}

impl Picture {
    /// The image in `bytes`, when they start as a PNG, JPEG, GIF or WebP
    /// file does.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Option<Self> {
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            "image/jpeg"
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            "image/gif"
        } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            "image/webp"
        } else {
            return None;
        };
        Some(Self {
            mime: mime.to_owned(),
            bytes,
        })
    }

    /// The image as base64, for a data URL.
    #[must_use]
    pub fn base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(&self.bytes)
    }

    /// The image whose base64 is `data`.
    #[must_use]
    pub fn from_base64(mime: &str, data: &str) -> Option<Self> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .ok()?;
        Some(Self {
            mime: mime.to_owned(),
            bytes,
        })
    }

    /// Its width and height, when it can be read.
    #[must_use]
    pub fn size(&self) -> Option<(u32, u32)> {
        image::ImageReader::new(Cursor::new(&self.bytes))
            .with_guessed_format()
            .ok()?
            .into_dimensions()
            .ok()
    }

    /// The image at half its width and height: a JPEG stays one, anything
    /// else becomes a PNG. `None` when it cannot be read, or is too small
    /// to halve.
    #[must_use]
    pub fn halved(&self) -> Option<Self> {
        let image = image::load_from_memory(&self.bytes).ok()?;
        let (width, height) = (image.width() / 2, image.height() / 2);
        if width == 0 || height == 0 {
            return None;
        }
        let smaller = image.resize(width, height, image::imageops::FilterType::Triangle);
        let (format, mime) = if self.mime == "image/jpeg" {
            (image::ImageFormat::Jpeg, "image/jpeg")
        } else {
            (image::ImageFormat::Png, "image/png")
        };
        let mut bytes = Vec::new();
        smaller
            .write_to(&mut Cursor::new(&mut bytes), format)
            .ok()?;
        Some(Self {
            mime: mime.to_owned(),
            bytes,
        })
    }
}

/// Reads the image at `path`, already resolved in the workspace.
pub(crate) fn read(path: &std::path::Path, shown: &std::path::Path) -> Result<Picture, ToolError> {
    let io = |source| ToolError::Io {
        path: shown.to_owned(),
        source,
    };
    let length = std::fs::metadata(path).map_err(io)?.len();
    if length > MOST_BYTES as u64 {
        return Err(ToolError::NotAnImage {
            path: shown.to_owned(),
            reason: format!("it holds {length} bytes, more than the 20 MB an image may"),
        });
    }
    let bytes = std::fs::read(path).map_err(io)?;
    Picture::from_bytes(bytes).ok_or_else(|| ToolError::NotAnImage {
        path: shown.to_owned(),
        reason: "it is not a PNG, JPEG, GIF or WebP image".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb([200, 30, 30]));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn an_image_is_told_by_its_first_bytes_and_halved() {
        let picture = Picture::from_bytes(png(64, 40)).unwrap();
        assert_eq!(picture.mime, "image/png");
        assert_eq!(picture.size(), Some((64, 40)));
        let half = picture.halved().unwrap();
        assert_eq!(half.size(), Some((32, 20)));
        let again = Picture::from_base64(&half.mime, &half.base64()).unwrap();
        assert_eq!(again, half);
        assert!(Picture::from_bytes(b"plain text".to_vec()).is_none());
        assert!(Picture::from_bytes(png(1, 1)).unwrap().halved().is_none());
    }
}
