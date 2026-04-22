use std::{borrow::Cow, time::Duration};

use smithay::{
    backend::{
        input::ButtonState,
        renderer::{
            element::{
                memory::MemoryRenderBufferRenderElement,
                solid::SolidColorRenderElement,
                surface::WaylandSurfaceRenderElement,
                AsRenderElements, Kind,
            },
            ImportAll, ImportMem, Renderer, Texture,
        },
    },
    desktop::{
        space::SpaceElement, utils::OutputPresentationFeedback, Window, WindowSurface, WindowSurfaceType,
    },
    input::{
        pointer::{
            AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
            GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
            GestureSwipeUpdateEvent, MotionEvent, PointerTarget, RelativeMotionEvent,
        },
        touch::TouchTarget,
        Seat,
    },
    output::Output,
    reexports::{
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    render_elements,
    utils::{user_data::UserDataMap, IsAlive, Logical, Physical, Point, Rectangle, Scale, Serial},
    wayland::{
        compositor::SurfaceData as WlSurfaceData,
        dmabuf::DmabufFeedback,
        seat::WaylandFocus,
        shell::xdg::XdgShellHandler,
    },
};

/// 0x110 = BTN_LEFT from linux/input-event-codes.h
const BTN_LEFT: u32 = 0x110;

/// Action determined from SSD button click, resolved before dispatching.
/// This allows dropping the RefCell<WindowState> borrow before calling into
/// AnvilState methods that may re-borrow the same window's decoration state
/// (e.g. maximize_request → decoration_state().is_ssd).
enum SsdAction {
    Close,
    Maximize,
    Move,
    Resize(super::grabs::ResizeEdge),
}

use super::ssd::HEADER_BAR_HEIGHT;
use super::ssd::input as ssd_input;
use crate::{focus::PointerFocusTarget, state::Backend, AnvilState};

use smithay::input::pointer::CursorImageStatus;

/// Map a resize edge to the appropriate named cursor icon.
fn cursor_for_resize_edge(edge: super::grabs::ResizeEdge) -> CursorImageStatus {
    use super::grabs::ResizeEdge;
    let name = match edge {
        ResizeEdge::TOP => "n-resize",
        ResizeEdge::BOTTOM => "s-resize",
        ResizeEdge::LEFT => "w-resize",
        ResizeEdge::RIGHT => "e-resize",
        ResizeEdge::TOP_LEFT => "nw-resize",
        ResizeEdge::TOP_RIGHT => "ne-resize",
        ResizeEdge::BOTTOM_LEFT => "sw-resize",
        ResizeEdge::BOTTOM_RIGHT => "se-resize",
        _ => return CursorImageStatus::default_named(),
    };
    CursorImageStatus::Named(name.parse().unwrap_or_default())
}

/// Compute the SSD hit zone from decoration state for cursor/action dispatch.
fn ssd_hit_zone(
    loc: Point<f64, Logical>,
    inner_geo: Rectangle<i32, Logical>,
    button_width: u32,
) -> ssd_input::DecorationHitZone {
    let w = inner_geo.size.w as f64;
    let h = (HEADER_BAR_HEIGHT + inner_geo.size.h) as f64;
    let bw = button_width as f64;
    ssd_input::hit_test(loc.x, loc.y, w, h, bw)
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowElement(pub Window);

impl WindowElement {
    pub fn surface_under(
        &self,
        location: Point<f64, Logical>,
        window_type: WindowSurfaceType,
    ) -> Option<(PointerFocusTarget, Point<i32, Logical>)> {
        let state = self.decoration_state();
        let offset = if state.is_ssd {
            Point::from((0, HEADER_BAR_HEIGHT))
        } else {
            Point::default()
        };

        // Check for popup/client surface first — popups extend beyond the
        // window and must take priority over SSD border zones.
        let surface_under = self.0.surface_under(location - offset.to_f64(), window_type);
        let client_hit = match self.0.underlying_surface() {
            WindowSurface::Wayland(_) => {
                surface_under.map(|(surface, loc)| (PointerFocusTarget::WlSurface(surface), loc + offset))
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(s) => {
                surface_under.map(|(_, loc)| (PointerFocusTarget::X11Surface(s.clone()), loc + offset))
            }
        };
        if client_hit.is_some() {
            return client_hit;
        }

        // No popup/client surface — check SSD zones
        if state.is_ssd {
            if location.y < HEADER_BAR_HEIGHT as f64 {
                return Some((PointerFocusTarget::SSD(SSD(self.clone())), Point::default()));
            }
            let window_geo = SpaceElement::geometry(&self.0);
            let w = window_geo.size.w as f64;
            let h = (HEADER_BAR_HEIGHT + window_geo.size.h) as f64;
            let bw = state.header_bar.button_width() as f64;
            let zone = ssd_input::hit_test(location.x, location.y, w, h, bw);
            if ssd_input::resize_edge_for_zone(zone).is_some() {
                return Some((PointerFocusTarget::SSD(SSD(self.clone())), Point::default()));
            }
        }

        None
    }

    pub fn with_surfaces<F>(&self, processor: F)
    where
        F: FnMut(&WlSurface, &WlSurfaceData),
    {
        self.0.with_surfaces(processor);
    }

    pub fn send_frame<T, F>(
        &self,
        output: &Output,
        time: T,
        throttle: Option<Duration>,
        primary_scan_out_output: F,
    ) where
        T: Into<Duration>,
        F: FnMut(&WlSurface, &WlSurfaceData) -> Option<Output> + Copy,
    {
        self.0.send_frame(output, time, throttle, primary_scan_out_output)
    }

    pub fn send_dmabuf_feedback<'a, P, F>(
        &self,
        output: &Output,
        primary_scan_out_output: P,
        select_dmabuf_feedback: F,
    ) where
        P: FnMut(&WlSurface, &WlSurfaceData) -> Option<Output> + Copy,
        F: Fn(&WlSurface, &WlSurfaceData) -> &'a DmabufFeedback + Copy,
    {
        self.0
            .send_dmabuf_feedback(output, primary_scan_out_output, select_dmabuf_feedback)
    }

    pub fn take_presentation_feedback<F1, F2>(
        &self,
        output_feedback: &mut OutputPresentationFeedback,
        primary_scan_out_output: F1,
        presentation_feedback_flags: F2,
    ) where
        F1: FnMut(&WlSurface, &WlSurfaceData) -> Option<Output> + Copy,
        F2: FnMut(&WlSurface, &WlSurfaceData) -> wp_presentation_feedback::Kind + Copy,
    {
        self.0.take_presentation_feedback(
            output_feedback,
            primary_scan_out_output,
            presentation_feedback_flags,
        )
    }

    #[cfg(feature = "xwayland")]
    #[inline]
    pub fn is_x11(&self) -> bool {
        self.0.is_x11()
    }

    #[inline]
    pub fn is_wayland(&self) -> bool {
        self.0.is_wayland()
    }

    #[inline]
    pub fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        self.0.wl_surface()
    }

    #[inline]
    pub fn user_data(&self) -> &UserDataMap {
        self.0.user_data()
    }
}

impl IsAlive for WindowElement {
    #[inline]
    fn alive(&self) -> bool {
        self.0.alive()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SSD(WindowElement);

impl IsAlive for SSD {
    #[inline]
    fn alive(&self) -> bool {
        self.0.alive()
    }
}

impl WaylandFocus for SSD {
    #[inline]
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        self.0.wl_surface()
    }
}

impl<BackendData: Backend> PointerTarget<AnvilState<BackendData>> for SSD {
    fn enter(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        data: &mut AnvilState<BackendData>,
        event: &MotionEvent,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
            let inner_geo = SpaceElement::geometry(&self.0 .0);
            let bw = state.header_bar.button_width();
            drop(state);
            let zone = ssd_hit_zone(event.location, inner_geo, bw);
            if let Some(edge) = ssd_input::resize_edge_for_zone(zone) {
                data.cursor_status = cursor_for_resize_edge(edge);
            } else {
                data.cursor_status = CursorImageStatus::default_named();
            }
        }
    }
    fn motion(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        data: &mut AnvilState<BackendData>,
        event: &MotionEvent,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
            let inner_geo = SpaceElement::geometry(&self.0 .0);
            let bw = state.header_bar.button_width();
            drop(state);
            let zone = ssd_hit_zone(event.location, inner_geo, bw);
            if let Some(edge) = ssd_input::resize_edge_for_zone(zone) {
                data.cursor_status = cursor_for_resize_edge(edge);
            } else {
                data.cursor_status = CursorImageStatus::default_named();
            }
        }
    }
    fn relative_motion(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &RelativeMotionEvent,
    ) {
    }
    fn button(
        &self,
        seat: &Seat<AnvilState<BackendData>>,
        data: &mut AnvilState<BackendData>,
        event: &ButtonEvent,
    ) {
        // Crash fix bugs 1+5: Only handle left-click press, not release or other buttons
        if event.state != ButtonState::Pressed || event.button != BTN_LEFT {
            return;
        }

        // Crash fix bug 4: Determine action while holding RefCell borrow, then drop it
        // before dispatching. maximize_request() calls decoration_state().is_ssd which
        // would re-borrow the same RefCell and panic.
        let action = {
            let state = self.0.decoration_state();
            if !state.is_ssd {
                return;
            }
            let loc = match state.header_bar.pointer_loc {
                Some(l) => l,
                None => return,
            };
            // Use hit_test for unified zone detection (borders, corners, buttons, title bar)
            let inner_geo = SpaceElement::geometry(&self.0 .0);
            let bw = state.header_bar.button_width();
            let zone = ssd_hit_zone(loc, inner_geo, bw);
            if let Some(edge) = ssd_input::resize_edge_for_zone(zone) {
                SsdAction::Resize(edge)
            } else {
                match zone {
                    ssd_input::DecorationHitZone::CloseButton => SsdAction::Close,
                    ssd_input::DecorationHitZone::MaximizeButton => SsdAction::Maximize,
                    _ => SsdAction::Move,
                }
            }
        }; // RefMut dropped — safe to call into AnvilState now

        let serial = event.serial;
        match action {
            SsdAction::Close => match self.0 .0.underlying_surface() {
                WindowSurface::Wayland(w) => w.send_close(),
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(w) => {
                    let _ = w.close();
                }
            },
            SsdAction::Maximize => match self.0 .0.underlying_surface() {
                WindowSurface::Wayland(w) => data.maximize_request(w.clone()),
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(w) => {
                    let surface = w.clone();
                    data.handle
                        .insert_idle(move |data| data.maximize_request_x11(&surface));
                }
            },
            SsdAction::Move => match self.0 .0.underlying_surface() {
                WindowSurface::Wayland(w) => {
                    let seat = seat.clone();
                    let toplevel = w.clone();
                    data.handle
                        .insert_idle(move |data| data.move_request_xdg(&toplevel, &seat, serial));
                }
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(w) => {
                    let window = w.clone();
                    data.handle
                        .insert_idle(move |data| data.move_request_x11(&window));
                }
            },
            SsdAction::Resize(edges) => {
                let seat = seat.clone();
                let window = self.0.clone();
                data.handle
                    .insert_idle(move |data| data.resize_request_ssd(&window, &seat, serial, edges));
            }
        }
    }
    fn axis(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _frame: AxisFrame,
    ) {
    }
    fn frame(&self, _seat: &Seat<AnvilState<BackendData>>, _data: &mut AnvilState<BackendData>) {}
    fn leave(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        data: &mut AnvilState<BackendData>,
        _serial: Serial,
        _time: u32,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_leave();
        }
        drop(state);
        data.cursor_status = CursorImageStatus::default_named();
    }
    fn gesture_swipe_begin(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GestureSwipeBeginEvent,
    ) {
    }
    fn gesture_swipe_update(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GestureSwipeUpdateEvent,
    ) {
    }
    fn gesture_swipe_end(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GestureSwipeEndEvent,
    ) {
    }
    fn gesture_pinch_begin(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GesturePinchBeginEvent,
    ) {
    }
    fn gesture_pinch_update(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GesturePinchUpdateEvent,
    ) {
    }
    fn gesture_pinch_end(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GesturePinchEndEvent,
    ) {
    }
    fn gesture_hold_begin(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GestureHoldBeginEvent,
    ) {
    }
    fn gesture_hold_end(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GestureHoldEndEvent,
    ) {
    }
}

impl<BackendData: Backend> TouchTarget<AnvilState<BackendData>> for SSD {
    fn down(
        &self,
        seat: &Seat<AnvilState<BackendData>>,
        data: &mut AnvilState<BackendData>,
        event: &smithay::input::touch::DownEvent,
        _seq: Serial,
    ) {
        // Crash fix bug 4: enter pointer + determine action, drop borrow, then dispatch
        let action = {
            let mut state = self.0.decoration_state();
            if !state.is_ssd {
                return;
            }
            state.header_bar.pointer_enter(event.location);
            // Touch down only starts move — close/maximize happen on touch_up
            if !state.header_bar.is_over_close() && !state.header_bar.is_over_maximize() {
                if state.header_bar.pointer_loc.is_some() {
                    Some(SsdAction::Move)
                } else {
                    None
                }
            } else {
                None
            }
        }; // RefMut dropped

        if let Some(SsdAction::Move) = action {
            let serial = event.serial;
            match self.0 .0.underlying_surface() {
                WindowSurface::Wayland(w) => {
                    let seat = seat.clone();
                    let toplevel = w.clone();
                    data.handle
                        .insert_idle(move |data| data.move_request_xdg(&toplevel, &seat, serial));
                }
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(w) => {
                    let window = w.clone();
                    data.handle
                        .insert_idle(move |data| data.move_request_x11(&window));
                }
            }
        }
    }

    fn up(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        data: &mut AnvilState<BackendData>,
        _event: &smithay::input::touch::UpEvent,
        _seq: Serial,
    ) {
        // Crash fix bug 4: determine action, drop borrow, then dispatch
        let action = {
            let state = self.0.decoration_state();
            if !state.is_ssd {
                return;
            }
            if state.header_bar.is_over_close() {
                Some(SsdAction::Close)
            } else if state.header_bar.is_over_maximize() {
                Some(SsdAction::Maximize)
            } else {
                None
            }
        }; // RefMut dropped

        if let Some(action) = action {
            match action {
                SsdAction::Close => match self.0 .0.underlying_surface() {
                    WindowSurface::Wayland(w) => w.send_close(),
                    #[cfg(feature = "xwayland")]
                    WindowSurface::X11(w) => {
                        let _ = w.close();
                    }
                },
                SsdAction::Maximize => match self.0 .0.underlying_surface() {
                    WindowSurface::Wayland(w) => data.maximize_request(w.clone()),
                    #[cfg(feature = "xwayland")]
                    WindowSurface::X11(w) => {
                        let surface = w.clone();
                        data.handle
                            .insert_idle(move |data| data.maximize_request_x11(&surface));
                    }
                },
                SsdAction::Move | SsdAction::Resize(_) => {} // Move/resize handled in touch_down, not up
            }
        }
    }

    fn motion(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        event: &smithay::input::touch::MotionEvent,
        _seq: Serial,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
        }
    }

    fn frame(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _seq: Serial,
    ) {
    }

    fn cancel(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _seq: Serial,
    ) {
    }

    fn shape(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &smithay::input::touch::ShapeEvent,
        _seq: Serial,
    ) {
    }

    fn orientation(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &smithay::input::touch::OrientationEvent,
        _seq: Serial,
    ) {
    }
}

impl SpaceElement for WindowElement {
    fn geometry(&self) -> Rectangle<i32, Logical> {
        let mut geo = SpaceElement::geometry(&self.0);
        if self.decoration_state().is_ssd {
            geo.size.h += HEADER_BAR_HEIGHT;
        }
        geo
    }
    fn bbox(&self) -> Rectangle<i32, Logical> {
        let mut bbox = SpaceElement::bbox(&self.0);
        if self.decoration_state().is_ssd {
            bbox.size.h += HEADER_BAR_HEIGHT;
        }
        bbox
    }
    fn is_in_input_region(&self, point: &Point<f64, Logical>) -> bool {
        if self.decoration_state().is_ssd {
            point.y < HEADER_BAR_HEIGHT as f64
                || SpaceElement::is_in_input_region(
                    &self.0,
                    &(*point - Point::from((0.0, HEADER_BAR_HEIGHT as f64))),
                )
        } else {
            SpaceElement::is_in_input_region(&self.0, point)
        }
    }
    fn z_index(&self) -> u8 {
        SpaceElement::z_index(&self.0)
    }

    fn set_activate(&self, activated: bool) {
        SpaceElement::set_activate(&self.0, activated);
        let mut state = self.decoration_state();
        if state.is_ssd {
            state.header_bar.is_focused = activated;
        }
    }
    fn output_enter(&self, output: &Output, overlap: Rectangle<i32, Logical>) {
        SpaceElement::output_enter(&self.0, output, overlap);
    }
    fn output_leave(&self, output: &Output) {
        SpaceElement::output_leave(&self.0, output);
    }
    #[profiling::function]
    fn refresh(&self) {
        SpaceElement::refresh(&self.0);
    }
}

render_elements!(
    pub WindowRenderElement<R> where R: ImportAll + ImportMem;
    Window=WaylandSurfaceRenderElement<R>,
    Decoration=SolidColorRenderElement,
    TitleBar=MemoryRenderBufferRenderElement<R>,
);

impl<R: Renderer> std::fmt::Debug for WindowRenderElement<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Window(arg0) => f.debug_tuple("Window").field(arg0).finish(),
            Self::Decoration(arg0) => f.debug_tuple("Decoration").field(arg0).finish(),
            Self::TitleBar(arg0) => f.debug_tuple("TitleBar").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

impl<R> AsRenderElements<R> for WindowElement
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + Texture + 'static,
{
    type RenderElement = WindowRenderElement<R>;

    fn render_elements<C: From<Self::RenderElement>>(
        &self,
        renderer: &mut R,
        mut location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
    ) -> Vec<C> {
        let window_bbox = SpaceElement::bbox(&self.0);

        if self.decoration_state().is_ssd && !window_bbox.is_empty() {
            let window_geo = SpaceElement::geometry(&self.0);
            let width = window_geo.size.w;
            let client_height = window_geo.size.h;

            let mut state = self.decoration_state();
            state.header_bar.redraw(width as u32, client_height);

            let mut vec: Vec<WindowRenderElement<R>> = Vec::new();

            // Window content + popups first — popups extend beyond the
            // window and must render ABOVE SSD borders (higher z = earlier in vec).
            let content_location_y = location.y + (scale.y * HEADER_BAR_HEIGHT as f64) as i32;
            let content_location = Point::from((location.x, content_location_y));
            let window_elements =
                AsRenderElements::render_elements(&self.0, renderer, content_location, scale, alpha);
            vec.extend(window_elements);

            // Title bar
            if let Some(ref buffer) = state.header_bar.title_bar_buffer {
                if let Ok(elem) = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    location.to_f64(),
                    buffer,
                    Some(alpha),
                    None,
                    None,
                    Kind::Unspecified,
                ) {
                    vec.push(WindowRenderElement::TitleBar(elem));
                }
            }

            // Border elements — lowest z, behind popups
            let border_elements =
                state
                    .header_bar
                    .border_render_elements(location, scale, alpha, client_height);
            vec.extend(border_elements.into_iter().map(WindowRenderElement::Decoration));

            drop(state);

            vec.into_iter().map(C::from).collect()
        } else {
            AsRenderElements::render_elements(&self.0, renderer, location, scale, alpha)
                .into_iter()
                .map(C::from)
                .collect()
        }
    }
}
