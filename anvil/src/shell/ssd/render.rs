//! Title bar pixmap rendering via tiny-skia.
//! Renders background, gradient stripe, button hover backgrounds, and button icons
//! into a single MemoryRenderBuffer.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::Transform as SmithayTransform;
use tiny_skia::{Color, LineCap, Paint, Pixmap, Rect, Stroke, Transform as TsTransform};

use super::icons;

/// Which button is currently hovered, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoverState {
    None,
    Close,
    Maximize,
}

/// Gradient stripe configuration, pre-parsed from theme.
#[derive(Debug, Clone)]
pub struct GradientConfig {
    pub height: u32,
    pub start_rgb: [u8; 3],
    pub end_rgb: [u8; 3],
}

/// Everything needed to render a title bar, extracted from theme at init time.
#[derive(Debug, Clone)]
pub struct TitleBarStyle {
    pub bg_focused: [f32; 4],
    pub bg_unfocused: [f32; 4],
    pub button_width: u32,
    pub close_hover_bg: [f32; 4],
    pub maximize_hover_bg: [f32; 4],
    pub gradient: Option<GradientConfig>,
    pub icon_color: [f32; 4],
    pub icon_color_unfocused: [f32; 4],
    pub icon_size: f32,
    pub icon_stroke_width: f32,
}

/// Render the entire title bar into a MemoryRenderBuffer.
pub fn render_title_bar(
    width: u32,
    height: u32,
    style: &TitleBarStyle,
    focused: bool,
    hover: HoverState,
) -> Option<MemoryRenderBuffer> {
    if width == 0 || height == 0 {
        return None;
    }

    let mut pixmap = Pixmap::new(width, height)?;

    // 1. Fill background
    let bg = if focused {
        style.bg_focused
    } else {
        style.bg_unfocused
    };
    pixmap.fill(rgba_f32_to_color(bg));

    // 2. Gradient stripe (focused only, if configured)
    if focused {
        if let Some(ref gradient) = style.gradient {
            draw_gradient_stripe(&mut pixmap, width, gradient);
        }
    }

    // 3. Button hover backgrounds
    let bw = style.button_width as f32;
    let bh = height as f32;
    match hover {
        HoverState::Close => {
            fill_button_bg(&mut pixmap, width as f32 - bw, bw, bh, style.close_hover_bg);
        }
        HoverState::Maximize => {
            fill_button_bg(
                &mut pixmap,
                width as f32 - bw * 2.0,
                bw,
                bh,
                style.maximize_hover_bg,
            );
        }
        HoverState::None => {}
    }

    // 4. Button icons (always visible)
    let icon_color = if focused {
        style.icon_color
    } else {
        style.icon_color_unfocused
    };
    let ts_icon_color = rgba_f32_to_color(icon_color);
    let half_icon = style.icon_size / 2.0;

    let mut paint = Paint::default();
    paint.set_color(ts_icon_color);
    paint.anti_alias = true;

    let stroke = Stroke {
        width: style.icon_stroke_width,
        line_cap: LineCap::Round,
        ..Stroke::default()
    };

    // Close icon (rightmost)
    let close_cx = width as f32 - bw / 2.0;
    let close_cy = bh / 2.0;
    if let Some(path) = icons::close_icon_path(close_cx, close_cy, half_icon) {
        pixmap.stroke_path(&path, &paint, &stroke, TsTransform::identity(), None);
    }

    // Maximize icon (second from right)
    let max_cx = width as f32 - bw * 1.5;
    let max_cy = bh / 2.0;
    if let Some(path) = icons::maximize_icon_path(max_cx, max_cy, half_icon) {
        pixmap.stroke_path(&path, &paint, &stroke, TsTransform::identity(), None);
    }

    // 5. Convert pixmap to MemoryRenderBuffer
    Some(MemoryRenderBuffer::from_slice(
        pixmap.data(),
        Fourcc::Abgr8888,
        (width as i32, height as i32),
        1,
        SmithayTransform::Normal,
        None,
    ))
}

fn fill_button_bg(pixmap: &mut Pixmap, x: f32, w: f32, h: f32, color: [f32; 4]) {
    if let Some(rect) = Rect::from_xywh(x, 0.0, w, h) {
        let c = rgba_f32_to_color(color);
        let mut paint = Paint::default();
        paint.set_color(c);
        pixmap.fill_rect(rect, &paint, TsTransform::identity(), None);
    }
}

fn draw_gradient_stripe(pixmap: &mut Pixmap, width: u32, gradient: &GradientConfig) {
    let h = gradient.height as f32;
    if h <= 0.0 {
        return;
    }

    let start =
        Color::from_rgba8(gradient.start_rgb[0], gradient.start_rgb[1], gradient.start_rgb[2], 255);
    let end = Color::from_rgba8(gradient.end_rgb[0], gradient.end_rgb[1], gradient.end_rgb[2], 255);

    let shader = tiny_skia::LinearGradient::new(
        tiny_skia::Point { x: 0.0, y: 0.0 },
        tiny_skia::Point {
            x: width as f32,
            y: 0.0,
        },
        vec![
            tiny_skia::GradientStop::new(0.0, start),
            tiny_skia::GradientStop::new(1.0, end),
        ],
        tiny_skia::SpreadMode::Pad,
        TsTransform::identity(),
    );

    if let Some(shader) = shader {
        if let Some(rect) = Rect::from_xywh(0.0, 0.0, width as f32, h) {
            let mut paint = Paint::default();
            paint.shader = shader;
            pixmap.fill_rect(rect, &paint, TsTransform::identity(), None);
        }
    }
}

/// Convert [f32; 4] RGBA (0.0-1.0 range) to tiny_skia::Color.
fn rgba_f32_to_color(c: [f32; 4]) -> Color {
    Color::from_rgba(c[0], c[1], c[2], c[3]).unwrap_or(Color::BLACK)
}
