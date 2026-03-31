use std::cell::RefCell;

use smithay::{
    desktop::{
        find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output, space::SpaceElement,
        PopupKeyboardGrab, PopupKind, PopupPointerGrab, PopupUngrabStrategy, Space, Window,
        WindowSurfaceType,
    },
    input::{pointer::Focus, Seat},
    output::Output,
    reexports::{
        wayland_protocols::xdg::{decoration as xdg_decoration, shell::server::xdg_toplevel},
        wayland_server::{
            protocol::{wl_output, wl_seat, wl_surface::WlSurface},
            Resource,
        },
    },
    utils::{Logical, Point, Rectangle, Serial},
    wayland::{
        compositor::{self, with_states},
        seat::WaylandFocus,
        shell::xdg::{
            Configure, PopupSurface, PositionerState, ToplevelCachedState, ToplevelSurface, XdgShellHandler,
            XdgShellState,
        },
    },
};
use tracing::{trace, warn};

use crate::{
    focus::KeyboardFocusTarget,
    shell::{TouchMoveSurfaceGrab, TouchResizeSurfaceGrab},
    state::{AnvilState, Backend},
    ClientState,
};

use super::{
    fullscreen_output_geometry, place_new_window, ssd::HEADER_BAR_HEIGHT, FullscreenSurface,
    PointerMoveSurfaceGrab, PointerResizeSurfaceGrab, ResizeData, ResizeEdge, ResizeState, SurfaceData,
    WindowElement,
};

impl<BackendData: Backend> XdgShellHandler for AnvilState<BackendData> {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Do not send a configure here, the initial configure
        // of a xdg_surface has to be sent during the commit if
        // the surface is not already configured
        let window = WindowElement(Window::new_wayland_window(surface.clone()));

        // Route window to the correct workspace based on client's workspace_id
        let workspace_id = surface.wl_surface().client()
            .and_then(|client| {
                let state = client.get_data::<ClientState>()?;
                *state.workspace_id.lock().unwrap()
            });

        let ws_id = workspace_id.unwrap_or(0);
        let space = match workspace_id.and_then(|id| self.workspaces.get_space_mut(id)) {
            Some(space) => space,
            None => self.workspaces.space_mut(),
        };
        if ws_id > 0 {
            // AI workspace: auto-maximize at output origin.
            // Configure client to fill the virtual output exactly (minus SSD header).
            // Element placed at output origin so header + content = output height.
            // Extract geometry from immutable borrow first, then mutate.
            let maximize_geo = space.outputs().next()
                .and_then(|o| {
                    let geo = space.output_geometry(o)?;
                    let zone = layer_map_for_output(o).non_exclusive_zone();
                    Some(Rectangle::new(geo.loc + zone.loc, zone.size))
                });

            if let Some(geometry) = maximize_geo {
                // Fullscreen: no SSD, full output size, Chromium --kiosk engages
                surface.with_pending_state(|state| {
                    state.states.set(xdg_toplevel::State::Fullscreen);
                    state.size = Some(geometry.size);
                });
                window.set_ssd(false);
                space.map_element(window.clone(), geometry.loc, true);
            } else {
                space.map_element(window.clone(), Point::default(), true);
            }
        } else {
            place_new_window(space, self.human_pointer.current_location(), &window, true);
        }

        compositor::add_post_commit_hook(surface.wl_surface(), |state: &mut Self, _, surface| {
            // Search all workspaces for the window
            if let Some(space) = state.workspaces.space_for_surface_mut(surface) {
                handle_toplevel_commit(space, surface);
            }
        });
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        // Do not send a configure here, the initial configure
        // of a xdg_surface has to be sent during the commit if
        // the surface is not already configured

        self.unconstrain_popup(&surface);

        if let Err(err) = self.popups.track_popup(PopupKind::from(surface)) {
            warn!("Failed to track popup: {}", err);
        }
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|state| {
            let geometry = positioner.get_geometry();
            state.geometry = geometry;
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let seat: Seat<AnvilState<BackendData>> = Seat::from_resource(&seat).unwrap();
        self.move_request_xdg(&surface, &seat, serial)
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let seat: Seat<AnvilState<BackendData>> = Seat::from_resource(&seat).unwrap();

        if let Some(touch) = seat.get_touch() {
            if touch.has_grab(serial) {
                let start_data = touch.grab_start_data().unwrap();
                tracing::info!(?start_data);

                // If the client disconnects after requesting a move
                // we can just ignore the request
                let Some(window) = self.window_for_surface(surface.wl_surface()) else {
                    tracing::info!("no window");
                    return;
                };

                // If the focus was for a different surface, ignore the request.
                if start_data.focus.is_none()
                    || !start_data
                        .focus
                        .as_ref()
                        .unwrap()
                        .0
                        .same_client_as(&surface.wl_surface().id())
                {
                    tracing::info!("different surface");
                    return;
                }
                let geometry = window.geometry();
                let loc = self.workspaces.space().element_location(&window).unwrap();
                let (initial_window_location, initial_window_size) = (loc, geometry.size);

                with_states(surface.wl_surface(), move |states| {
                    states
                        .data_map
                        .get::<RefCell<SurfaceData>>()
                        .unwrap()
                        .borrow_mut()
                        .resize_state = ResizeState::Resizing(ResizeData {
                        edges: edges.into(),
                        initial_window_location,
                        initial_window_size,
                    });
                });

                let grab = TouchResizeSurfaceGrab {
                    start_data,
                    window,
                    edges: edges.into(),
                    initial_window_location,
                    initial_window_size,
                    last_window_size: initial_window_size,
                    ssd_height_offset: 0,
                };

                touch.set_grab(self, grab, serial);
                return;
            }
        }

        let pointer = seat.get_pointer().unwrap();

        // Check that this surface has a click grab.
        if !pointer.has_grab(serial) {
            return;
        }

        let start_data = pointer.grab_start_data().unwrap();

        let window = self.window_for_surface(surface.wl_surface()).unwrap();

        // If the focus was for a different surface, ignore the request.
        if start_data.focus.is_none()
            || !start_data
                .focus
                .as_ref()
                .unwrap()
                .0
                .same_client_as(&surface.wl_surface().id())
        {
            return;
        }

        let geometry = window.geometry();
        let loc = self.workspaces.space().element_location(&window).unwrap();
        let (initial_window_location, initial_window_size) = (loc, geometry.size);

        with_states(surface.wl_surface(), move |states| {
            states
                .data_map
                .get::<RefCell<SurfaceData>>()
                .unwrap()
                .borrow_mut()
                .resize_state = ResizeState::Resizing(ResizeData {
                edges: edges.into(),
                initial_window_location,
                initial_window_size,
            });
        });

        let grab = PointerResizeSurfaceGrab {
            start_data,
            window,
            edges: edges.into(),
            initial_window_location,
            initial_window_size,
            last_window_size: initial_window_size,
            ssd_height_offset: 0,
        };

        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    fn ack_configure(&mut self, surface: WlSurface, configure: Configure) {
        if let Configure::Toplevel(configure) = configure {
            if let Some(serial) = with_states(&surface, |states| {
                if let Some(data) = states.data_map.get::<RefCell<SurfaceData>>() {
                    if let ResizeState::WaitingForFinalAck(_, serial) = data.borrow().resize_state {
                        return Some(serial);
                    }
                }

                None
            }) {
                // When the resize grab is released the surface
                // resize state will be set to WaitingForFinalAck
                // and the client will receive a configure request
                // without the resize state to inform the client
                // resizing has finished. Here we will wait for
                // the client to acknowledge the end of the
                // resizing. To check if the surface was resizing
                // before sending the configure we need to use
                // the current state as the received acknowledge
                // will no longer have the resize state set
                let is_resizing = with_states(&surface, |states| {
                    states
                        .cached_state
                        .get::<ToplevelCachedState>()
                        .current()
                        .last_acked
                        .as_ref()
                        .is_some_and(|c| c.state.states.contains(xdg_toplevel::State::Resizing))
                });

                if configure.serial >= serial && is_resizing {
                    with_states(&surface, |states| {
                        let mut data = states
                            .data_map
                            .get::<RefCell<SurfaceData>>()
                            .unwrap()
                            .borrow_mut();
                        if let ResizeState::WaitingForFinalAck(resize_data, _) = data.resize_state {
                            data.resize_state = ResizeState::WaitingForCommit(resize_data);
                        } else {
                            unreachable!()
                        }
                    });
                }
            }

            let window = self
                .workspaces.space()
                .elements()
                .find(|element| element.wl_surface().as_deref() == Some(&surface));
            if let Some(window) = window {
                use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
                // Suppress SSD for fullscreen windows — ack_configure fires on every
                // configure, so without this check SSD would re-enable during fullscreen
                let is_fullscreen = configure
                    .state
                    .states
                    .contains(xdg_toplevel::State::Fullscreen);
                let is_ssd = if is_fullscreen {
                    false
                } else {
                    configure
                        .state
                        .decoration_mode
                        .map(|mode| mode == Mode::ServerSide)
                        .unwrap_or(true)
                };
                window.set_ssd(is_ssd);
            }
        }
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, mut wl_output: Option<wl_output::WlOutput>) {
        // NOTE: This is only one part of the solution. We can set the
        // location and configure size here, but the surface should be rendered fullscreen
        // independently from its buffer size
        let wl_surface = surface.wl_surface();

        // Find which workspace contains this surface, then operate on that space
        let ws_id = self.workspaces.workspace_id_for_surface(wl_surface);
        let space = match ws_id.and_then(|id| self.workspaces.get_space_mut(id)) {
            Some(space) => space,
            None => self.workspaces.space_mut(),
        };

        let output_geometry = fullscreen_output_geometry(wl_surface, wl_output.as_ref(), space);

        if let Some(geometry) = output_geometry {
            let space = match ws_id.and_then(|id| self.workspaces.get_space(id)) {
                Some(space) => space,
                None => self.workspaces.space(),
            };
            let output = wl_output
                .as_ref()
                .and_then(Output::from_resource)
                .unwrap_or_else(|| space.outputs().next().unwrap().clone());
            let client = match self.display_handle.get_client(wl_surface.id()) {
                Ok(client) => client,
                Err(_) => return,
            };
            for output in output.client_outputs(&client) {
                wl_output = Some(output);
            }
            let window = self.workspaces.window_for_surface(wl_surface).unwrap();

            // Suppress SSD for fullscreen windows — no decorations in fullscreen
            window.set_ssd(false);

            surface.with_pending_state(|state| {
                state.states.unset(xdg_toplevel::State::Maximized);
                state.states.set(xdg_toplevel::State::Fullscreen);
                state.size = Some(geometry.size);
                state.fullscreen_output = wl_output;
            });
            output.user_data().insert_if_missing(FullscreenSurface::default);
            output
                .user_data()
                .get::<FullscreenSurface>()
                .unwrap()
                .set(window.clone());
            trace!("Fullscreening: {:?}", window);

            // Reposition element to fullscreen geometry origin — same pattern as maximize_request
            let space = match ws_id.and_then(|id| self.workspaces.get_space_mut(id)) {
                Some(space) => space,
                None => self.workspaces.space_mut(),
            };
            space.map_element(window, geometry.loc, true);
        }

        // The protocol demands us to always reply with a configure,
        // regardless of we fulfilled the request or not
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        } else {
            // Will be sent during initial configure
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let ret = surface.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Fullscreen);
            state.size = None;
            state.fullscreen_output.take()
        });
        if let Some(output) = ret {
            let output = Output::from_resource(&output).unwrap();
            if let Some(fullscreen) = output.user_data().get::<FullscreenSurface>() {
                trace!("Unfullscreening: {:?}", fullscreen.get());
                fullscreen.clear();
                self.backend_data.reset_buffers(&output);
            }
        }

        // The protocol demands us to always reply with a configure,
        // regardless of we fulfilled the request or not
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        } else {
            // Will be sent during initial configure
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        // NOTE: This should use layer-shell when it is implemented to
        // get the correct maximum size
        // Crash fix bug 2: replace unwrap/expect with safe early returns
        let Some(window) = self.window_for_surface(surface.wl_surface()) else {
            tracing::warn!("maximize_request: window not found in space, ignoring");
            return;
        };
        let ws_id = self.workspaces.workspace_id_for_surface(surface.wl_surface());
        let space = match ws_id.and_then(|id| self.workspaces.get_space(id)) {
            Some(space) => space,
            None => self.workspaces.space(),
        };
        let outputs_for_window = space.outputs_for_element(&window);
        let Some(output) = outputs_for_window
            .first()
            .or_else(|| space.outputs().next())
        else {
            tracing::warn!("maximize_request: no outputs found, ignoring");
            return;
        };
        let Some(geo) = space.output_geometry(output) else {
            tracing::warn!("maximize_request: output has no geometry, ignoring");
            return;
        };
        let geometry = {
            let map = layer_map_for_output(output);
            let zone = map.non_exclusive_zone();
            Rectangle::new(geo.loc + zone.loc, zone.size)
        };

        // For SSD windows, subtract decoration height from the client size
        // so that title_bar + client = usable zone height.
        let is_ssd = window.decoration_state().is_ssd;
        let client_size = if is_ssd {
            (geometry.size.w, geometry.size.h - HEADER_BAR_HEIGHT).into()
        } else {
            geometry.size
        };

        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Maximized);
            state.size = Some(client_size);
        });
        let space = match ws_id.and_then(|id| self.workspaces.get_space_mut(id)) {
            Some(space) => space,
            None => self.workspaces.space_mut(),
        };
        space.map_element(window, geometry.loc, true);

        // The protocol demands us to always reply with a configure,
        // regardless of we fulfilled the request or not
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        } else {
            // Will be sent during initial configure
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Maximized);
            state.size = None;
        });

        // The protocol demands us to always reply with a configure,
        // regardless of we fulfilled the request or not
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        } else {
            // Will be sent during initial configure
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let seat: Seat<AnvilState<BackendData>> = Seat::from_resource(&seat).unwrap();
        let kind = PopupKind::Xdg(surface);
        if let Some(root) = find_popup_root_surface(&kind).ok().and_then(|root| {
            self.workspaces.space()
                .elements()
                .find(|w| w.wl_surface().map(|s| *s == root).unwrap_or(false))
                .cloned()
                .map(KeyboardFocusTarget::from)
                .or_else(|| {
                    self.workspaces.space()
                        .outputs()
                        .find_map(|o| {
                            let map = layer_map_for_output(o);
                            map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL).cloned()
                        })
                        .map(KeyboardFocusTarget::LayerSurface)
                })
        }) {
            let ret = self.popups.grab_popup(root, kind, &seat, serial);

            if let Ok(mut grab) = ret {
                if let Some(keyboard) = seat.get_keyboard() {
                    if keyboard.is_grabbed()
                        && !(keyboard.has_grab(serial)
                            || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
                    {
                        grab.ungrab(PopupUngrabStrategy::All);
                        return;
                    }
                    keyboard.set_focus(self, grab.current_grab(), serial);
                    keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
                }
                if let Some(pointer) = seat.get_pointer() {
                    if pointer.is_grabbed()
                        && !(pointer.has_grab(serial)
                            || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
                    {
                        grab.ungrab(PopupUngrabStrategy::All);
                        return;
                    }
                    pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
                }
            }
        }
    }
}

impl<BackendData: Backend> AnvilState<BackendData> {
    pub fn move_request_xdg(&mut self, surface: &ToplevelSurface, seat: &Seat<Self>, serial: Serial) {
        if let Some(touch) = seat.get_touch() {
            if touch.has_grab(serial) {
                // Crash fix bug 3: replace unwrap with safe early returns
                let Some(start_data) = touch.grab_start_data() else {
                    return;
                };

                let Some(window) = self.window_for_surface(surface.wl_surface()) else {
                    return;
                };

                // If the focus was for a different surface, ignore the request.
                if start_data.focus.is_none()
                    || !start_data
                        .focus
                        .as_ref()
                        .unwrap()
                        .0
                        .same_client_as(&surface.wl_surface().id())
                {
                    return;
                }

                let Some(mut initial_window_location) = self.workspaces.space().element_location(&window) else {
                    return;
                };

                // If surface is maximized then unmaximize it
                let changed = surface.with_pending_state(|state| {
                    if state.states.unset(xdg_toplevel::State::Maximized) {
                        state.size = None;
                        true
                    } else {
                        false
                    }
                });
                if changed {
                    surface.send_configure();

                    // NOTE: In real compositor mouse location should be mapped to a new window size
                    // For example, you could:
                    // 1) transform mouse pointer position from compositor space to window space (location relative)
                    // 2) divide the x coordinate by width of the window to get the percentage
                    //   - 0.0 would be on the far left of the window
                    //   - 0.5 would be in middle of the window
                    //   - 1.0 would be on the far right of the window
                    // 3) multiply the percentage by new window width
                    // 4) by doing that, drag will look a lot more natural
                    //
                    // but for anvil needs setting location to pointer location is fine
                    initial_window_location = start_data.location.to_i32_round();
                }

                let grab = TouchMoveSurfaceGrab {
                    start_data,
                    window,
                    initial_window_location,
                };

                touch.set_grab(self, grab, serial);
                return;
            }
        }

        // Crash fix bug 3: replace unwrap with safe early returns
        let Some(pointer) = seat.get_pointer() else {
            return;
        };

        // Check that this surface has a click grab.
        if !pointer.has_grab(serial) {
            return;
        }

        let Some(start_data) = pointer.grab_start_data() else {
            return;
        };

        // If the client disconnects after requesting a move
        // we can just ignore the request
        let Some(window) = self.window_for_surface(surface.wl_surface()) else {
            return;
        };

        // If the focus was for a different surface, ignore the request.
        if start_data.focus.is_none()
            || !start_data
                .focus
                .as_ref()
                .unwrap()
                .0
                .same_client_as(&surface.wl_surface().id())
        {
            return;
        }

        let Some(mut initial_window_location) = self.workspaces.space().element_location(&window) else {
            return;
        };

        // If surface is maximized then unmaximize it
        let changed = surface.with_pending_state(|state| {
            if state.states.unset(xdg_toplevel::State::Maximized) {
                state.size = None;
                true
            } else {
                false
            }
        });
        if changed {
            surface.send_configure();

            // NOTE: In real compositor mouse location should be mapped to a new window size
            // For example, you could:
            // 1) transform mouse pointer position from compositor space to window space (location relative)
            // 2) divide the x coordinate by width of the window to get the percentage
            //   - 0.0 would be on the far left of the window
            //   - 0.5 would be in middle of the window
            //   - 1.0 would be on the far right of the window
            // 3) multiply the percentage by new window width
            // 4) by doing that, drag will look a lot more natural
            //
            // but for anvil needs setting location to pointer location is fine
            let pos = pointer.current_location();
            initial_window_location = (pos.x as i32, pos.y as i32).into();
        }

        let grab = PointerMoveSurfaceGrab {
            start_data,
            window,
            initial_window_location,
        };

        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    /// Compositor-initiated resize from SSD border/corner zones.
    pub fn resize_request_ssd(
        &mut self,
        window: &WindowElement,
        seat: &Seat<Self>,
        serial: Serial,
        edges: ResizeEdge,
    ) {
        let Some(pointer) = seat.get_pointer() else {
            return;
        };

        if !pointer.has_grab(serial) {
            return;
        }

        let Some(start_data) = pointer.grab_start_data() else {
            return;
        };

        // Use full geometry (including SSD) for initial_window_size so that
        // the position adjustment in the grab's release handler is consistent
        // with window.geometry() which also includes SSD.
        let geometry = window.geometry();
        let Some(initial_window_location) = self.workspaces.space().element_location(window) else {
            return;
        };
        let initial_window_size = geometry.size;

        // ssd_height_offset: the grab subtracts this from configure sizes
        // so the client receives client-only dimensions.
        let ssd_height_offset = super::ssd::HEADER_BAR_HEIGHT;

        // Set resize state on the surface
        if let Some(surface) = window.wl_surface() {
            with_states(&surface, |states| {
                if let Some(data) = states.data_map.get::<RefCell<SurfaceData>>() {
                    data.borrow_mut().resize_state = ResizeState::Resizing(ResizeData {
                        edges,
                        initial_window_location,
                        initial_window_size,
                    });
                }
            });
        }

        let grab = PointerResizeSurfaceGrab {
            start_data,
            window: window.clone(),
            edges,
            initial_window_location,
            initial_window_size,
            last_window_size: initial_window_size,
            ssd_height_offset,
        };

        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self.window_for_surface(&root) else {
            return;
        };

        let mut outputs_for_window = self.workspaces.space().outputs_for_element(&window);
        if outputs_for_window.is_empty() {
            return;
        }

        // Get a union of all outputs' geometries.
        let mut outputs_geo = self
            .workspaces.space()
            .output_geometry(&outputs_for_window.pop().unwrap())
            .unwrap();
        for output in outputs_for_window {
            outputs_geo = outputs_geo.merge(self.workspaces.space().output_geometry(&output).unwrap());
        }

        let window_geo = self.workspaces.space().element_geometry(&window).unwrap();

        // The target geometry for the positioner should be relative to its parent's geometry, so
        // we will compute that here.
        let mut target = outputs_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

/// Should be called on `WlSurface::commit` of xdg toplevel
fn handle_toplevel_commit(space: &mut Space<WindowElement>, surface: &WlSurface) -> Option<()> {
    let window = space
        .elements()
        .find(|w| w.wl_surface().as_deref() == Some(surface))
        .cloned()?;

    let mut window_loc = space.element_location(&window)?;
    let geometry = window.geometry();

    let new_loc: Point<Option<i32>, Logical> = with_states(window.wl_surface().as_deref()?, |states| {
        let data = states.data_map.get::<RefCell<SurfaceData>>()?.borrow_mut();

        if let ResizeState::Resizing(resize_data) = data.resize_state {
            let edges = resize_data.edges;
            let loc = resize_data.initial_window_location;
            let size = resize_data.initial_window_size;

            // If the window is being resized by top or left, its location must be adjusted
            // accordingly.
            edges.intersects(ResizeEdge::TOP_LEFT).then(|| {
                let new_x = edges
                    .intersects(ResizeEdge::LEFT)
                    .then_some(loc.x + (size.w - geometry.size.w));

                let new_y = edges
                    .intersects(ResizeEdge::TOP)
                    .then_some(loc.y + (size.h - geometry.size.h));

                (new_x, new_y).into()
            })
        } else {
            None
        }
    })?;

    if let Some(new_x) = new_loc.x {
        window_loc.x = new_x;
    }
    if let Some(new_y) = new_loc.y {
        window_loc.y = new_y;
    }

    if new_loc.x.is_some() || new_loc.y.is_some() {
        // If TOP or LEFT side of the window got resized, we have to move it
        space.map_element(window, window_loc, false);
    }

    Some(())
}
