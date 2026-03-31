pub mod geometry;
pub mod icons;
pub mod input;
pub mod render;
pub mod theme;

pub use theme::DecorationTheme;

use smithay::{
    backend::renderer::element::solid::{SolidColorBuffer, SolidColorRenderElement},
    backend::renderer::element::memory::MemoryRenderBuffer,
    desktop::WindowSurface,
    input::Seat,
    utils::{Logical, Physical, Point, Scale, Serial},
    wayland::shell::xdg::XdgShellHandler,
};

use std::cell::{RefCell, RefMut};

use crate::{state::Backend, AnvilState};

use super::WindowElement;

use render::{GradientConfig, HoverState, TitleBarStyle};

pub struct WindowState {
    pub is_ssd: bool,
    pub header_bar: HeaderBar,
}

#[derive(Debug, Clone)]
pub struct HeaderBar {
    pub pointer_loc: Option<Point<f64, Logical>>,
    pub width: u32,
    pub is_focused: bool,
    pub close_button_hover: bool,
    pub maximize_button_hover: bool,
    // Title bar rendered as a single buffer via tiny-skia
    pub title_bar_buffer: Option<MemoryRenderBuffer>,
    // Borders (still SolidColorBuffer)
    pub border_left: SolidColorBuffer,
    pub border_right: SolidColorBuffer,
    pub border_bottom: SolidColorBuffer,
    // Cached config from theme
    config: HeaderConfig,
    // Track last rendered state for dirty checking
    rendered_width: u32,
    rendered_focused: bool,
    rendered_hover: HoverState,
}

/// Cached configuration from DecorationTheme, stored per-window.
#[derive(Debug, Clone)]
struct HeaderConfig {
    style: TitleBarStyle,
    border_focused: [f32; 4],
    border_unfocused: [f32; 4],
    border_width: i32,
    button_width: i32,
}

impl Default for HeaderConfig {
    fn default() -> Self {
        Self::from_theme(&DecorationTheme::load())
    }
}

impl HeaderConfig {
    fn from_theme(theme: &DecorationTheme) -> Self {
        let gradient = if theme.gradient_stripe.enabled {
            Some(GradientConfig {
                height: theme.gradient_stripe.height as u32,
                start_rgb: hex_to_rgb(&theme.gradient_stripe.start_color)
                    .unwrap_or([0x44, 0x0D, 0xC3]),
                end_rgb: hex_to_rgb(&theme.gradient_stripe.end_color)
                    .unwrap_or([0x00, 0xBF, 0x63]),
            })
        } else {
            None
        };

        Self {
            style: TitleBarStyle {
                bg_focused: theme.title_bar.bg_focused,
                bg_unfocused: theme.title_bar.bg_unfocused,
                button_width: theme.buttons.width as u32,
                close_hover_bg: theme.buttons.close_hover_bg,
                maximize_hover_bg: theme.buttons.maximize_hover_bg,
                gradient,
                icon_color: theme.button_icon.color,
                icon_color_unfocused: theme.button_icon.color_unfocused,
                icon_size: theme.button_icon.size as f32,
                icon_stroke_width: theme.button_icon.stroke_width,
            },
            border_focused: theme.border.color_focused,
            border_unfocused: theme.border.color_unfocused,
            border_width: theme.border.width,
            button_width: theme.buttons.width,
        }
    }
}

pub const HEADER_BAR_HEIGHT: i32 = 32;

/// Parse hex color to RGB bytes.
fn hex_to_rgb(hex: &str) -> Option<[u8; 3]> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some([r, g, b])
}

impl HeaderBar {
    /// Create a new HeaderBar with colors from the given theme.
    pub fn new(theme: &DecorationTheme) -> Self {
        Self {
            pointer_loc: None,
            width: 0,
            is_focused: true,
            close_button_hover: false,
            maximize_button_hover: false,
            title_bar_buffer: None,
            border_left: SolidColorBuffer::default(),
            border_right: SolidColorBuffer::default(),
            border_bottom: SolidColorBuffer::default(),
            config: HeaderConfig::from_theme(theme),
            rendered_width: 0,
            rendered_focused: false,
            rendered_hover: HoverState::None,
        }
    }

    pub fn button_width(&self) -> u32 {
        self.config.button_width as u32
    }

    pub fn border_width(&self) -> i32 {
        self.config.border_width
    }

    pub fn border_color(&self) -> [f32; 4] {
        if self.is_focused {
            self.config.border_focused
        } else {
            self.config.border_unfocused
        }
    }

    pub fn pointer_enter(&mut self, loc: Point<f64, Logical>) {
        self.pointer_loc = Some(loc);
    }

    pub fn pointer_leave(&mut self) {
        self.pointer_loc = None;
    }

    /// Check if pointer is over close button (rightmost)
    pub fn is_over_close(&self) -> bool {
        let bw = self.button_width();
        self.pointer_loc
            .as_ref()
            .map(|l| l.x >= (self.width - bw) as f64)
            .unwrap_or(false)
    }

    /// Check if pointer is over maximize button (second from right)
    pub fn is_over_maximize(&self) -> bool {
        let bw = self.button_width();
        self.pointer_loc
            .as_ref()
            .map(|l| l.x >= (self.width - bw * 2) as f64 && l.x < (self.width - bw) as f64)
            .unwrap_or(false)
    }

    pub fn clicked<BackendData: Backend>(
        &mut self,
        seat: &Seat<AnvilState<BackendData>>,
        state: &mut AnvilState<BackendData>,
        window: &WindowElement,
        serial: Serial,
    ) {
        if self.is_over_close() {
            match window.0.underlying_surface() {
                WindowSurface::Wayland(w) => w.send_close(),
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(w) => {
                    let _ = w.close();
                }
            };
        } else if self.is_over_maximize() {
            match window.0.underlying_surface() {
                WindowSurface::Wayland(w) => state.maximize_request(w.clone()),
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(w) => {
                    let surface = w.clone();
                    state
                        .handle
                        .insert_idle(move |data| data.maximize_request_x11(&surface));
                }
            };
        } else if self.pointer_loc.is_some() {
            // Title bar drag → move window
            match window.0.underlying_surface() {
                WindowSurface::Wayland(w) => {
                    let seat = seat.clone();
                    let toplevel = w.clone();
                    state
                        .handle
                        .insert_idle(move |data| data.move_request_xdg(&toplevel, &seat, serial));
                }
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(w) => {
                    let window = w.clone();
                    state
                        .handle
                        .insert_idle(move |data| data.move_request_x11(&window));
                }
            };
        }
    }

    pub fn touch_down<BackendData: Backend>(
        &mut self,
        seat: &Seat<AnvilState<BackendData>>,
        state: &mut AnvilState<BackendData>,
        window: &WindowElement,
        serial: Serial,
    ) {
        if !self.is_over_close() && !self.is_over_maximize() {
            if self.pointer_loc.is_some() {
                match window.0.underlying_surface() {
                    WindowSurface::Wayland(w) => {
                        let seat = seat.clone();
                        let toplevel = w.clone();
                        state
                            .handle
                            .insert_idle(move |data| data.move_request_xdg(&toplevel, &seat, serial));
                    }
                    #[cfg(feature = "xwayland")]
                    WindowSurface::X11(w) => {
                        let window = w.clone();
                        state
                            .handle
                            .insert_idle(move |data| data.move_request_x11(&window));
                    }
                };
            }
        }
    }

    pub fn touch_up<BackendData: Backend>(
        &mut self,
        _seat: &Seat<AnvilState<BackendData>>,
        state: &mut AnvilState<BackendData>,
        window: &WindowElement,
        _serial: Serial,
    ) {
        if self.is_over_close() {
            match window.0.underlying_surface() {
                WindowSurface::Wayland(w) => w.send_close(),
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(w) => {
                    let _ = w.close();
                }
            };
        } else if self.is_over_maximize() {
            match window.0.underlying_surface() {
                WindowSurface::Wayland(w) => state.maximize_request(w.clone()),
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(w) => {
                    let surface = w.clone();
                    state
                        .handle
                        .insert_idle(move |data| data.maximize_request_x11(&surface));
                }
            };
        }
    }

    pub fn redraw(&mut self, width: u32, client_height: i32) {
        if width == 0 {
            self.width = 0;
            self.title_bar_buffer = None;
            return;
        }

        // Update width first (used by is_over_* methods)
        self.width = width;

        // Determine current hover state
        let hover = if self.is_over_close() {
            HoverState::Close
        } else if self.is_over_maximize() {
            HoverState::Maximize
        } else {
            HoverState::None
        };

        // Re-render title bar if state changed
        if width != self.rendered_width
            || self.is_focused != self.rendered_focused
            || hover != self.rendered_hover
        {
            self.title_bar_buffer = render::render_title_bar(
                width,
                HEADER_BAR_HEIGHT as u32,
                &self.config.style,
                self.is_focused,
                hover,
            );
            self.rendered_width = width;
            self.rendered_focused = self.is_focused;
            self.rendered_hover = hover;
        }

        // Update hover tracking
        self.close_button_hover = hover == HoverState::Close;
        self.maximize_button_hover = hover == HoverState::Maximize;

        // Borders
        let border_color = self.border_color();
        let border_w = self.border_width();
        if border_w > 0 && client_height > 0 {
            self.border_left
                .update((border_w, client_height), border_color);
            self.border_right
                .update((border_w, client_height), border_color);
            self.border_bottom
                .update((width as i32, border_w), border_color);
        }
    }
}

impl HeaderBar {
    /// Render border elements (left, right, bottom) around the client surface.
    pub fn border_render_elements(
        &self,
        location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
        client_height: i32,
    ) -> Vec<SolidColorRenderElement> {
        let bw = self.border_width();
        if bw <= 0 || client_height <= 0 || self.width == 0 {
            return vec![];
        }

        let left_offset: Point<i32, Logical> = Point::from((0, HEADER_BAR_HEIGHT));
        let right_offset: Point<i32, Logical> =
            Point::from((self.width as i32 - bw, HEADER_BAR_HEIGHT));
        let bottom_offset: Point<i32, Logical> =
            Point::from((0, HEADER_BAR_HEIGHT + client_height - bw));

        vec![
            SolidColorRenderElement::from_buffer(
                &self.border_left,
                location + left_offset.to_physical_precise_round(scale),
                scale,
                alpha,
                smithay::backend::renderer::element::Kind::Unspecified,
            ),
            SolidColorRenderElement::from_buffer(
                &self.border_right,
                location + right_offset.to_physical_precise_round(scale),
                scale,
                alpha,
                smithay::backend::renderer::element::Kind::Unspecified,
            ),
            SolidColorRenderElement::from_buffer(
                &self.border_bottom,
                location + bottom_offset.to_physical_precise_round(scale),
                scale,
                alpha,
                smithay::backend::renderer::element::Kind::Unspecified,
            ),
        ]
    }
}

impl WindowElement {
    pub fn decoration_state(&self) -> RefMut<'_, WindowState> {
        self.user_data().insert_if_missing(|| {
            RefCell::new(WindowState {
                is_ssd: false,
                header_bar: HeaderBar {
                    pointer_loc: None,
                    width: 0,
                    is_focused: true,
                    close_button_hover: false,
                    maximize_button_hover: false,
                    title_bar_buffer: None,
                    border_left: SolidColorBuffer::default(),
                    border_right: SolidColorBuffer::default(),
                    border_bottom: SolidColorBuffer::default(),
                    config: HeaderConfig::default(),
                    rendered_width: 0,
                    rendered_focused: false,
                    rendered_hover: HoverState::None,
                },
            })
        });

        self.user_data()
            .get::<RefCell<WindowState>>()
            .unwrap()
            .borrow_mut()
    }

    pub fn set_ssd(&self, ssd: bool) {
        self.decoration_state().is_ssd = ssd;
    }
}
