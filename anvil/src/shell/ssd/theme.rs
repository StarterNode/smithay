use serde::Deserialize;
use std::path::Path;
use tracing::{info, warn};

const WINDOW_PATH: &str = "/etc/gui/window.json";

/// Design tokens for window decorations.
/// All color values are pre-computed [f32; 4] RGBA arrays.
/// Loaded from /etc/gui/window.json (compiled by gui-compile).
/// Falls back to hardcoded defaults if file is missing.
#[derive(Debug, Clone)]
pub struct DecorationTheme {
    pub title_bar: TitleBarTheme,
    pub buttons: ButtonsTheme,
    pub border: BorderTheme,
    pub resize: ResizeTheme,
    pub snap_preview: SnapPreviewTheme,
    pub gradient_stripe: GradientStripeTheme,
    pub button_icon: ButtonIconTheme,
}

#[derive(Debug, Clone)]
pub struct TitleBarTheme {
    pub height: i32,
    pub bg_focused: [f32; 4],
    pub bg_unfocused: [f32; 4],
    pub text_color_focused: [f32; 4],
    pub text_color_unfocused: [f32; 4],
    pub font_family: String,
    pub font_size: f32,
    pub font_weight: u16,
    pub text_padding_left: i32,
}

#[derive(Debug, Clone)]
pub struct ButtonsTheme {
    pub width: i32,
    pub close_hover_bg: [f32; 4],
    pub maximize_hover_bg: [f32; 4],
    pub minimize_hover_bg: [f32; 4],
}

#[derive(Debug, Clone)]
pub struct BorderTheme {
    pub width: i32,
    pub color_focused: [f32; 4],
    pub color_unfocused: [f32; 4],
    pub color_default: [f32; 4],
}

#[derive(Debug, Clone)]
pub struct ResizeTheme {
    pub edge_width: i32,
    pub corner_size: i32,
}

#[derive(Debug, Clone)]
pub struct SnapPreviewTheme {
    pub color: [f32; 4],
    pub opacity: f32,
    pub border_radius: i32,
    pub trigger_distance: i32,
}

#[derive(Debug, Clone)]
pub struct GradientStripeTheme {
    pub enabled: bool,
    pub height: i32,
    pub start_color: String,
    pub end_color: String,
}

#[derive(Debug, Clone)]
pub struct ButtonIconTheme {
    pub color: [f32; 4],
    pub color_unfocused: [f32; 4],
    pub size: i32,
    pub stroke_width: f32,
}

// --- JSON deserialization: /etc/gui/window.json (pre-computed by gui-compile) ---

#[derive(Deserialize)]
struct WindowFile {
    title_bar: Option<WinTitleBar>,
    buttons: Option<WinButtons>,
    button_icon: Option<WinButtonIcon>,
    border: Option<WinBorder>,
    gradient_stripe: Option<WinGradientStripe>,
    snap_preview: Option<WinSnapPreview>,
}

#[derive(Deserialize)]
struct WinTitleBar {
    height: Option<i32>,
    font_size: Option<f32>,
    font_weight: Option<u16>,
    text_padding_left: Option<i32>,
    font_family: Option<String>,
    bg_focused: Option<[f32; 4]>,
    bg_unfocused: Option<[f32; 4]>,
    text_focused: Option<[f32; 4]>,
    text_unfocused: Option<[f32; 4]>,
}

#[derive(Deserialize)]
struct WinButtons {
    width: Option<i32>,
    close_hover_bg: Option<[f32; 4]>,
    maximize_hover_bg: Option<[f32; 4]>,
    minimize_hover_bg: Option<[f32; 4]>,
}

#[derive(Deserialize)]
struct WinButtonIcon {
    size: Option<i32>,
    stroke_width: Option<f32>,
    color_focused: Option<[f32; 4]>,
    color_unfocused: Option<[f32; 4]>,
}

#[derive(Deserialize)]
struct WinBorder {
    width: Option<i32>,
    resize_edge_width: Option<i32>,
    resize_corner_size: Option<i32>,
    color_focused: Option<[f32; 4]>,
    color_unfocused: Option<[f32; 4]>,
    color_default: Option<[f32; 4]>,
}

#[derive(Deserialize)]
struct WinGradientStripe {
    enabled: Option<bool>,
    height: Option<i32>,
    start_color: Option<String>,
    end_color: Option<String>,
}

#[derive(Deserialize)]
struct WinSnapPreview {
    opacity: Option<f32>,
    border_radius: Option<i32>,
    trigger_distance: Option<i32>,
    color: Option<[f32; 4]>,
}

// --- Hardcoded defaults matching gui-compile output from theme.json v1.0.0 palette ---

const DEF_TB_BG_FOCUSED: [f32; 4] = [0.09921569, 0.08745098, 0.13058823, 1.0];
const DEF_TB_BG_UNFOCUSED: [f32; 4] = [0.039215688, 0.02745098, 0.07058824, 1.0];
const DEF_TEXT_FOCUSED: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const DEF_TEXT_UNFOCUSED: [f32; 4] = [0.4, 0.4, 0.4, 1.0];
const DEF_CLOSE_HOVER: [f32; 4] = [0.8980392, 0.34901962, 0.21176471, 1.0];
const DEF_MAX_HOVER: [f32; 4] = [0.3192157, 0.3701961, 0.74666667, 1.0];
const DEF_BORDER_FOCUSED: [f32; 4] = [0.23921569, 0.2901961, 0.6666667, 1.0];
const DEF_BORDER_UNFOCUSED: [f32; 4] = [0.09921569, 0.08745098, 0.13058823, 1.0];
const DEF_BORDER_DEFAULT: [f32; 4] = [0.07921569, 0.06745098, 0.11058824, 1.0];
const DEF_SNAP_COLOR: [f32; 4] = [0.23921569, 0.2901961, 0.6666667, 1.0];

impl DecorationTheme {
    /// Load decoration theme from /etc/gui/window.json (pre-computed colors).
    /// Falls back to hardcoded defaults if file is missing or unparseable.
    pub fn load() -> Self {
        let path = Path::new(WINDOW_PATH);
        if !path.exists() {
            warn!("Window theme {} not found, using defaults", WINDOW_PATH);
            return Self::defaults();
        }

        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to read {}: {}", WINDOW_PATH, e);
                return Self::defaults();
            }
        };

        let file: WindowFile = match serde_json::from_str(&contents) {
            Ok(f) => f,
            Err(e) => {
                warn!("Failed to parse {}: {}", WINDOW_PATH, e);
                return Self::defaults();
            }
        };

        info!("Loaded window theme from {}", WINDOW_PATH);

        let tb = file.title_bar.as_ref();
        let btn = file.buttons.as_ref();
        let bi = file.button_icon.as_ref();
        let bdr = file.border.as_ref();
        let gs = file.gradient_stripe.as_ref();
        let sp = file.snap_preview.as_ref();

        Self {
            title_bar: TitleBarTheme {
                height: tb.and_then(|t| t.height).unwrap_or(32),
                bg_focused: tb.and_then(|t| t.bg_focused).unwrap_or(DEF_TB_BG_FOCUSED),
                bg_unfocused: tb.and_then(|t| t.bg_unfocused).unwrap_or(DEF_TB_BG_UNFOCUSED),
                text_color_focused: tb.and_then(|t| t.text_focused).unwrap_or(DEF_TEXT_FOCUSED),
                text_color_unfocused: tb.and_then(|t| t.text_unfocused).unwrap_or(DEF_TEXT_UNFOCUSED),
                font_family: tb.and_then(|t| t.font_family.clone()).unwrap_or_else(|| "IBM Plex Mono".into()),
                font_size: tb.and_then(|t| t.font_size).unwrap_or(14.0),
                font_weight: tb.and_then(|t| t.font_weight).unwrap_or(500),
                text_padding_left: tb.and_then(|t| t.text_padding_left).unwrap_or(12),
            },
            buttons: ButtonsTheme {
                width: btn.and_then(|b| b.width).unwrap_or(32),
                close_hover_bg: btn.and_then(|b| b.close_hover_bg).unwrap_or(DEF_CLOSE_HOVER),
                maximize_hover_bg: btn.and_then(|b| b.maximize_hover_bg).unwrap_or(DEF_MAX_HOVER),
                minimize_hover_bg: btn.and_then(|b| b.minimize_hover_bg).unwrap_or(DEF_MAX_HOVER),
            },
            border: BorderTheme {
                width: bdr.and_then(|b| b.width).unwrap_or(2),
                color_focused: bdr.and_then(|b| b.color_focused).unwrap_or(DEF_BORDER_FOCUSED),
                color_unfocused: bdr.and_then(|b| b.color_unfocused).unwrap_or(DEF_BORDER_UNFOCUSED),
                color_default: bdr.and_then(|b| b.color_default).unwrap_or(DEF_BORDER_DEFAULT),
            },
            resize: ResizeTheme {
                edge_width: bdr.and_then(|b| b.resize_edge_width).unwrap_or(6),
                corner_size: bdr.and_then(|b| b.resize_corner_size).unwrap_or(12),
            },
            snap_preview: SnapPreviewTheme {
                color: sp.and_then(|s| s.color).unwrap_or(DEF_SNAP_COLOR),
                opacity: sp.and_then(|s| s.opacity).unwrap_or(0.2),
                border_radius: sp.and_then(|s| s.border_radius).unwrap_or(16),
                trigger_distance: sp.and_then(|s| s.trigger_distance).unwrap_or(16),
            },
            gradient_stripe: GradientStripeTheme {
                enabled: gs.and_then(|g| g.enabled).unwrap_or(true),
                height: gs.and_then(|g| g.height).unwrap_or(3),
                start_color: gs.and_then(|g| g.start_color.clone()).unwrap_or_else(|| "#440DC3".into()),
                end_color: gs.and_then(|g| g.end_color.clone()).unwrap_or_else(|| "#00bf63".into()),
            },
            button_icon: ButtonIconTheme {
                color: bi.and_then(|i| i.color_focused).unwrap_or(DEF_TEXT_FOCUSED),
                color_unfocused: bi.and_then(|i| i.color_unfocused).unwrap_or(DEF_TEXT_UNFOCUSED),
                size: bi.and_then(|i| i.size).unwrap_or(10),
                stroke_width: bi.and_then(|i| i.stroke_width).unwrap_or(1.5),
            },
        }
    }

    /// Hardcoded defaults matching gui-compile output from theme.json v1.0.0 palette.
    pub fn defaults() -> Self {
        Self {
            title_bar: TitleBarTheme {
                height: 32,
                bg_focused: DEF_TB_BG_FOCUSED,
                bg_unfocused: DEF_TB_BG_UNFOCUSED,
                text_color_focused: DEF_TEXT_FOCUSED,
                text_color_unfocused: DEF_TEXT_UNFOCUSED,
                font_family: "IBM Plex Mono".into(),
                font_size: 14.0,
                font_weight: 500,
                text_padding_left: 12,
            },
            buttons: ButtonsTheme {
                width: 32,
                close_hover_bg: DEF_CLOSE_HOVER,
                maximize_hover_bg: DEF_MAX_HOVER,
                minimize_hover_bg: DEF_MAX_HOVER,
            },
            border: BorderTheme {
                width: 2,
                color_focused: DEF_BORDER_FOCUSED,
                color_unfocused: DEF_BORDER_UNFOCUSED,
                color_default: DEF_BORDER_DEFAULT,
            },
            resize: ResizeTheme {
                edge_width: 6,
                corner_size: 12,
            },
            snap_preview: SnapPreviewTheme {
                color: DEF_SNAP_COLOR,
                opacity: 0.2,
                border_radius: 16,
                trigger_distance: 16,
            },
            gradient_stripe: GradientStripeTheme {
                enabled: true,
                height: 3,
                start_color: "#440DC3".into(),
                end_color: "#00bf63".into(),
            },
            button_icon: ButtonIconTheme {
                color: DEF_TEXT_FOCUSED,
                color_unfocused: DEF_TEXT_UNFOCUSED,
                size: 10,
                stroke_width: 1.5,
            },
        }
    }
}
