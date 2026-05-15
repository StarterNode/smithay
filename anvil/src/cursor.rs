use std::{collections::HashMap, io::Read, time::Duration};

use tracing::warn;
use xcursor::{
    parser::{parse_xcursor, Image},
    CursorTheme,
};

static FALLBACK_CURSOR_DATA: &[u8] = include_bytes!("../resources/cursor.rgba");

/// WINDOW-CHROME-002 Phase 2.7: shapes preloaded at startup so cursor_status
/// = Named(<shape>) actually renders the requested shape (not always "default").
/// "default" is mandatory — it's the fallback when a non-preloaded shape is
/// requested AND the per-shape resize cursors used by anvil's SSD hover handlers.
const PRELOAD_SHAPES: &[&str] = &[
    "default",
    "w-resize",
    "e-resize",
    "n-resize",
    "s-resize",
    "nw-resize",
    "ne-resize",
    "sw-resize",
    "se-resize",
];

pub struct Cursor {
    icons: HashMap<String, Vec<Image>>,
    size: u32,
}

impl Cursor {
    pub fn load() -> Cursor {
        let name = std::env::var("XCURSOR_THEME")
            .ok()
            .unwrap_or_else(|| "default".into());
        let size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(24);

        let theme = CursorTheme::load(&name);
        let mut icons: HashMap<String, Vec<Image>> = HashMap::new();
        for shape in PRELOAD_SHAPES {
            match load_icon(&theme, shape) {
                Ok(images) => {
                    icons.insert((*shape).to_string(), images);
                }
                Err(err) => {
                    warn!("Unable to load xcursor shape '{}': {} — will fall back to default", shape, err);
                }
            }
        }
        // Guarantee "default" exists even if theme lookup failed across the board.
        icons.entry("default".to_string()).or_insert_with(|| {
            vec![Image {
                size: 32,
                width: 64,
                height: 64,
                xhot: 1,
                yhot: 1,
                delay: 1,
                pixels_rgba: Vec::from(FALLBACK_CURSOR_DATA),
                pixels_argb: vec![], //unused
            }]
        });

        Cursor { icons, size }
    }

    /// Returns the "default" cursor frame. Kept for backward compatibility with
    /// call sites (XWayland init) that don't track a specific shape.
    pub fn get_image(&self, scale: u32, time: Duration) -> Image {
        self.get_image_for("default", scale, time)
    }

    /// Returns the frame for the requested CSS-style shape name (e.g. "w-resize").
    /// Falls back to "default" if the shape wasn't preloaded or isn't in the theme.
    pub fn get_image_for(&self, shape: &str, scale: u32, time: Duration) -> Image {
        let size = self.size * scale;
        let images = self
            .icons
            .get(shape)
            .or_else(|| self.icons.get("default"))
            .expect("default cursor always present after Cursor::load");
        frame(time.as_millis() as u32, size, images)
    }
}

fn nearest_images(size: u32, images: &[Image]) -> impl Iterator<Item = &Image> {
    // Follow the nominal size of the cursor to choose the nearest
    let nearest_image = images
        .iter()
        .min_by_key(|image| (size as i32 - image.size as i32).abs())
        .unwrap();

    images
        .iter()
        .filter(move |image| image.width == nearest_image.width && image.height == nearest_image.height)
}

fn frame(mut millis: u32, size: u32, images: &[Image]) -> Image {
    let total = nearest_images(size, images).fold(0, |acc, image| acc + image.delay);
    if total == 0 {
        return nearest_images(size, images).next().unwrap().clone();
    }
    millis %= total;

    for img in nearest_images(size, images) {
        if millis < img.delay {
            return img.clone();
        }
        millis -= img.delay;
    }

    unreachable!()
}

#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("Theme has no default cursor")]
    NoDefaultCursor,
    #[error("Error opening xcursor file: {0}")]
    File(#[from] std::io::Error),
    #[error("Failed to parse XCursor file")]
    Parse,
}

fn load_icon(theme: &CursorTheme, name: &str) -> Result<Vec<Image>, Error> {
    let icon_path = theme.load_icon(name).ok_or(Error::NoDefaultCursor)?;
    let mut cursor_file = std::fs::File::open(icon_path)?;
    let mut cursor_data = Vec::new();
    cursor_file.read_to_end(&mut cursor_data)?;
    parse_xcursor(&cursor_data).ok_or(Error::Parse)
}
