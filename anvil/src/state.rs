#[cfg(feature = "xwayland")]
use std::os::unix::io::OwnedFd;
use std::{
    collections::HashMap,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

use compstr::ipc::IpcHandler;
use tracing::{info, warn};

use smithay::{
    backend::{
        input::TabletToolDescriptor,
        renderer::element::{
            default_primary_scanout_output_compare, utils::select_dmabuf_feedback, RenderElementStates,
        },
    },
    delegate_compositor, delegate_data_control, delegate_data_device, delegate_fixes,
    delegate_fractional_scale, delegate_input_method_manager, delegate_kde_decoration,
    delegate_keyboard_shortcuts_inhibit,
    delegate_layer_shell, delegate_pointer_constraints, delegate_pointer_gestures,
    delegate_presentation, delegate_primary_selection, delegate_relative_pointer,
    delegate_security_context, delegate_shm, delegate_tablet_manager, delegate_text_input_manager,
    delegate_viewporter, delegate_virtual_keyboard_manager, delegate_xdg_activation, delegate_xdg_decoration,
    delegate_xdg_shell,
    desktop::{
        space::SpaceElement,
        utils::{
            surface_presentation_feedback_flags_from_states, surface_primary_scanout_output,
            update_surface_primary_scanout_output, with_surfaces_surface_tree, OutputPresentationFeedback,
        },
        PopupKind, PopupManager, Space,
    },
    input::{
        dnd::{DnDGrab, DndGrabHandler, DndTarget, GrabType, Source},
        keyboard::{Keysym, LedState, XkbConfig},
        pointer::{CursorImageStatus, Focus, PointerHandle},
        Seat, SeatHandler, SeatState,
    },
    output::Output,
    reexports::{
        calloop::{generic::Generic, Interest, LoopHandle, Mode, PostAction},
        wayland_protocols::xdg::decoration::{
            self as xdg_decoration, zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
        },
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            delegate_dispatch,
            protocol::{
                wl_keyboard::WlKeyboard,
                wl_pointer::WlPointer,
                wl_seat::WlSeat,
                wl_surface::WlSurface,
                wl_touch::WlTouch,
            },
            Client, DataInit, Display, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    utils::{Clock, Logical, Monotonic, Point, Rectangle, Serial, Time},
    wayland::{
        commit_timing::{CommitTimerBarrierStateUserData, CommitTimingManagerState},
        compositor::{get_parent, with_states, CompositorClientState, CompositorHandler, CompositorState},
        dmabuf::DmabufFeedback,
        fifo::{FifoBarrierCachedState, FifoManagerState},
        fixes::FixesState,
        fractional_scale::{with_fractional_scale, FractionalScaleHandler, FractionalScaleManagerState},
        image_capture_source::{
            ImageCaptureSource, ImageCaptureSourceHandler, ImageCaptureSourceState,
            OutputCaptureSourceHandler, OutputCaptureSourceState,
        },
        image_copy_capture::{
            BufferConstraints, Frame, ImageCopyCaptureHandler, ImageCopyCaptureState, Session, SessionRef,
        },
        input_method::{InputMethodHandler, InputMethodManagerState, PopupSurface},
        keyboard_shortcuts_inhibit::{
            KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
        },
        output::{OutputHandler, OutputManagerState},
        pointer_constraints::{with_pointer_constraint, PointerConstraintsHandler, PointerConstraintsState},
        pointer_gestures::PointerGesturesState,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        seat::{KeyboardUserData, PointerUserData, SeatGlobalData, SeatUserData, TouchUserData, WaylandFocus},
        security_context::{
            SecurityContext, SecurityContextHandler, SecurityContextListenerSource, SecurityContextState,
        },
        selection::{
            data_device::{set_data_device_focus, DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler},
            primary_selection::{set_primary_focus, PrimarySelectionHandler, PrimarySelectionState},
            wlr_data_control::{DataControlHandler, DataControlState},
            SelectionHandler,
        },
        shell::{
            kde::decoration::{KdeDecorationHandler, KdeDecorationState},
            wlr_layer::WlrLayerShellState,
            xdg::{
                decoration::{XdgDecorationHandler, XdgDecorationState},
                ToplevelSurface, XdgShellState,
            },
        },
        shm::{ShmHandler, ShmState},
        single_pixel_buffer::SinglePixelBufferState,
        socket::ListeningSocketSource,
        tablet_manager::{TabletManagerState, TabletSeatHandler},
        text_input::TextInputManagerState,
        viewporter::ViewporterState,
        virtual_keyboard::VirtualKeyboardManagerState,
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
        xdg_foreign::{XdgForeignHandler, XdgForeignState},
    },
};

#[cfg(feature = "xwayland")]
use crate::cursor::Cursor;
use crate::{
    focus::{KeyboardFocusTarget, PointerFocusTarget},
    shell::{snap::SnapPreview, ssd::DecorationTheme, WindowElement},
};
#[cfg(feature = "xwayland")]
use smithay::{
    delegate_xwayland_keyboard_grab, delegate_xwayland_shell,
    utils::Size,
    wayland::selection::{SelectionSource, SelectionTarget},
    wayland::xwayland_keyboard_grab::{XWaylandKeyboardGrabHandler, XWaylandKeyboardGrabState},
    wayland::xwayland_shell,
    xwayland::{X11Wm, XWayland, XWaylandEvent},
};

#[derive(Debug, Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    pub security_context: Option<SecurityContext>,
    /// Workspace this client belongs to. None = desktop (workspace 0).
    /// Some(id) = AI workspace. Set at connection time via workspace-specific socket.
    /// Interior mutability via Mutex because ClientState is behind Arc.
    pub workspace_id: std::sync::Mutex<Option<crate::workspace::WorkspaceId>>,
}
impl ClientData for ClientState {
    /// Notification that a client was initialized
    fn initialized(&self, _client_id: ClientId) {}
    /// Notification that a client is disconnected
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

#[derive(Debug)]
pub struct AnvilState<BackendData: Backend + 'static> {
    pub backend_data: BackendData,
    pub socket_name: Option<String>,
    pub display_handle: DisplayHandle,
    pub running: Arc<AtomicBool>,
    pub handle: LoopHandle<'static, AnvilState<BackendData>>,

    // desktop
    pub workspaces: crate::workspace::WorkspaceManager<crate::shell::WindowElement>,
    pub mirror: crate::workspace::MirrorState,
    pub export: crate::workspace::ExportState,
    pub cockpit_socket: compstr::socket::CockpitSocket,
    pub popups: PopupManager,

    // smithay state
    pub compositor_state: CompositorState,
    pub data_device_state: DataDeviceState,
    pub layer_shell_state: WlrLayerShellState,
    pub output_manager_state: OutputManagerState,
    pub primary_selection_state: PrimarySelectionState,
    pub data_control_state: DataControlState,
    pub seat_state: SeatState<AnvilState<BackendData>>,
    pub keyboard_shortcuts_inhibit_state: KeyboardShortcutsInhibitState,
    pub shm_state: ShmState,
    pub viewporter_state: ViewporterState,
    pub xdg_activation_state: XdgActivationState,
    pub xdg_decoration_state: XdgDecorationState,
    pub kde_decoration_state: KdeDecorationState,
    pub xdg_shell_state: XdgShellState,
    pub presentation_state: PresentationState,
    pub fractional_scale_manager_state: FractionalScaleManagerState,
    pub xdg_foreign_state: XdgForeignState,
    #[cfg(feature = "xwayland")]
    pub xwayland_shell_state: xwayland_shell::XWaylandShellState,
    pub single_pixel_buffer_state: SinglePixelBufferState,
    pub fifo_manager_state: FifoManagerState,
    pub commit_timing_manager_state: CommitTimingManagerState,
    pub image_capture_source_state: ImageCaptureSourceState,
    pub output_capture_source_state: OutputCaptureSourceState,
    pub image_copy_capture_state: ImageCopyCaptureState,

    pub dnd_icon: Option<DndIcon>,

    // input-related fields
    pub suppressed_keys: Vec<Keysym>,
    pub cursor_status: CursorImageStatus,
    pub ai_cursor_status: CursorImageStatus,
    pub human_seat_name: String,
    pub human_seat: Seat<AnvilState<BackendData>>,
    pub clock: Clock<Monotonic>,
    pub human_pointer: PointerHandle<AnvilState<BackendData>>,
    pub ai_seat: Seat<AnvilState<BackendData>>,
    pub ai_pointer: PointerHandle<AnvilState<BackendData>>,
    /// Copilot mode — when set, physical input routes to ai_seat for direct
    /// human control of AI workspace. None = normal (human_seat). Some(id) = copilot.
    pub copilot_mode: Option<crate::workspace::WorkspaceId>,

    #[cfg(feature = "xwayland")]
    pub xwm: Option<X11Wm>,
    #[cfg(feature = "xwayland")]
    pub xdisplay: Option<u32>,

    #[cfg(feature = "debug")]
    pub renderdoc: Option<renderdoc::RenderDoc<renderdoc::V141>>,

    pub show_window_preview: bool,
    pub decoration_theme: DecorationTheme,
    pub snap_preview: Option<SnapPreview>,

    // IPC — workspace-specific Wayland sockets for client tagging
    pub workspace_sockets: HashMap<crate::workspace::WorkspaceId, String>,
}

#[derive(Debug)]
pub struct DndIcon {
    pub surface: WlSurface,
    pub offset: Point<i32, Logical>,
}

delegate_compositor!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> DataDeviceHandler for AnvilState<BackendData> {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl<BackendData: Backend> WaylandDndGrabHandler for AnvilState<BackendData> {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        self.dnd_icon = icon.map(|surface| DndIcon {
            surface,
            offset: (0, 0).into(),
        });

        match type_ {
            GrabType::Pointer => {
                let pointer = seat.get_pointer().unwrap();
                let start_data = pointer.grab_start_data().unwrap();
                pointer.set_grab(
                    self,
                    DnDGrab::new_pointer(&self.display_handle, start_data, source, seat),
                    serial,
                    Focus::Keep,
                );
            }
            GrabType::Touch => {
                let touch = seat.get_touch().unwrap();
                let start_data = touch.grab_start_data().unwrap();
                touch.set_grab(
                    self,
                    DnDGrab::new_touch(&self.display_handle, start_data, source, seat),
                    serial,
                );
            }
        }
    }
}

impl<BackendData: Backend> DndGrabHandler for AnvilState<BackendData> {
    fn dropped(
        &mut self,
        _target: Option<DndTarget<'_, Self>>,
        _validated: bool,
        _seat: Seat<Self>,
        _location: Point<f64, Logical>,
    ) {
        self.dnd_icon = None;
    }
}
delegate_data_device!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> OutputHandler for AnvilState<BackendData> {}

// Manual output dispatch — replaces delegate_output! to override can_view().
// Workspace N clients see only the virtual "AI-Desktop" output.
// Workspace 0 (desktop) clients see only the physical output.
const _: () = {
    use smithay::reexports::wayland_protocols::xdg::xdg_output::zv1::server::{
        zxdg_output_manager_v1::ZxdgOutputManagerV1, zxdg_output_v1::ZxdgOutputV1,
    };
    use smithay::reexports::wayland_server::{
        delegate_dispatch, delegate_global_dispatch, protocol::wl_output::WlOutput, Client,
        DataInit, DisplayHandle, GlobalDispatch, New,
    };
    use smithay::wayland::output::{
        OutputManagerState, OutputUserData, WlOutputData, XdgOutputUserData,
    };

    // 1. GlobalDispatch<WlOutput> — custom can_view for output isolation
    impl<BackendData: Backend + 'static> GlobalDispatch<WlOutput, WlOutputData>
        for AnvilState<BackendData>
    {
        fn bind(
            state: &mut Self,
            dh: &DisplayHandle,
            client: &Client,
            resource: New<WlOutput>,
            global_data: &WlOutputData,
            data_init: &mut DataInit<'_, Self>,
        ) {
            <OutputManagerState as GlobalDispatch<WlOutput, WlOutputData, Self>>::bind(
                state, dh, client, resource, global_data, data_init,
            )
        }

        fn can_view(client: Client, global_data: &WlOutputData) -> bool {
            let output_name = global_data.output.name();
            let is_virtual = output_name == "AI-Desktop";
            let client_ws = client
                .get_data::<ClientState>()
                .and_then(|cs| cs.workspace_id.lock().ok().and_then(|g| *g));

            let result = match (is_virtual, client_ws) {
                // Virtual output — only visible to workspace N clients (id > 0)
                (true, Some(id)) if id > 0 => true,
                (true, _) => false,
                // Physical output — only visible to desktop clients (workspace 0 / None)
                (false, None) | (false, Some(0)) => true,
                (false, _) => false,
            };

            tracing::info!(
                "can_view: output={}, is_virtual={}, client_ws={:?}, result={}",
                output_name, is_virtual, client_ws, result,
            );

            result
        }
    }

    // 2-5. Remaining dispatches — delegate as before
    delegate_global_dispatch!(@<BackendData: Backend + 'static> AnvilState<BackendData>: [ZxdgOutputManagerV1: ()] => OutputManagerState);
    delegate_dispatch!(@<BackendData: Backend + 'static> AnvilState<BackendData>: [WlOutput: OutputUserData] => OutputManagerState);
    delegate_dispatch!(@<BackendData: Backend + 'static> AnvilState<BackendData>: [ZxdgOutputV1: XdgOutputUserData] => OutputManagerState);
    delegate_dispatch!(@<BackendData: Backend + 'static> AnvilState<BackendData>: [ZxdgOutputManagerV1: ()] => OutputManagerState);
};

impl<BackendData: Backend> SelectionHandler for AnvilState<BackendData> {
    type SelectionUserData = ();

    #[cfg(feature = "xwayland")]
    fn new_selection(&mut self, ty: SelectionTarget, source: Option<SelectionSource>, _seat: Seat<Self>) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.new_selection(ty, source.map(|source| source.mime_types())) {
                warn!(?err, ?ty, "Failed to set Xwayland selection");
            }
        }
    }

    #[cfg(feature = "xwayland")]
    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        _user_data: &(),
    ) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.send_selection(ty, mime_type, fd) {
                warn!(?err, "Failed to send primary (X11 -> Wayland)");
            }
        }
    }
}

impl<BackendData: Backend> PrimarySelectionHandler for AnvilState<BackendData> {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}
delegate_primary_selection!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> DataControlHandler for AnvilState<BackendData> {
    fn data_control_state(&mut self) -> &mut DataControlState {
        &mut self.data_control_state
    }
}

delegate_data_control!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> ShmHandler for AnvilState<BackendData> {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
delegate_shm!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> SeatHandler for AnvilState<BackendData> {
    type KeyboardFocus = KeyboardFocusTarget;
    type PointerFocus = PointerFocusTarget;
    type TouchFocus = PointerFocusTarget;

    fn seat_state(&mut self) -> &mut SeatState<AnvilState<BackendData>> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, target: Option<&KeyboardFocusTarget>) {
        let dh = &self.display_handle;

        let wl_surface = target.and_then(WaylandFocus::wl_surface);

        let focus = wl_surface.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, focus.clone());
        set_primary_focus(dh, seat, focus);
    }
    fn cursor_image(&mut self, seat: &Seat<Self>, image: CursorImageStatus) {
        if *seat == self.human_seat {
            self.cursor_status = image;
        } else if *seat == self.ai_seat {
            self.ai_cursor_status = image;
        }
    }

    fn led_state_changed(&mut self, _seat: &Seat<Self>, led_state: LedState) {
        self.backend_data.update_led_state(led_state)
    }
}
// Manual GlobalDispatch for WlSeat — custom can_view() for two-seat filtering
impl<BackendData: Backend + 'static> GlobalDispatch<WlSeat, SeatGlobalData<AnvilState<BackendData>>>
    for AnvilState<BackendData>
{
    fn bind(
        state: &mut Self,
        dh: &DisplayHandle,
        client: &Client,
        resource: New<WlSeat>,
        global_data: &SeatGlobalData<Self>,
        data_init: &mut DataInit<'_, Self>,
    ) {
        <SeatState<Self> as GlobalDispatch<WlSeat, SeatGlobalData<Self>, Self>>::bind(
            state, dh, client, resource, global_data, data_init,
        )
    }

    fn can_view(client: Client, global_data: &SeatGlobalData<Self>) -> bool {
        let workspace_id = client
            .get_data::<ClientState>()
            .and_then(|cs| *cs.workspace_id.lock().unwrap());
        crate::workspace::seat_can_view(workspace_id, global_data.seat_name())
    }
}

// Keep the 4 Dispatch impls via delegate macros
delegate_dispatch!(@<BackendData: Backend + 'static> AnvilState<BackendData>: [WlSeat: SeatUserData<AnvilState<BackendData>>] => SeatState<AnvilState<BackendData>>);
delegate_dispatch!(@<BackendData: Backend + 'static> AnvilState<BackendData>: [WlPointer: PointerUserData<AnvilState<BackendData>>] => SeatState<AnvilState<BackendData>>);
delegate_dispatch!(@<BackendData: Backend + 'static> AnvilState<BackendData>: [WlKeyboard: KeyboardUserData<AnvilState<BackendData>>] => SeatState<AnvilState<BackendData>>);
delegate_dispatch!(@<BackendData: Backend + 'static> AnvilState<BackendData>: [WlTouch: TouchUserData<AnvilState<BackendData>>] => SeatState<AnvilState<BackendData>>);

impl<BackendData: Backend> TabletSeatHandler for AnvilState<BackendData> {
    fn tablet_tool_image(&mut self, _tool: &TabletToolDescriptor, image: CursorImageStatus) {
        // TODO: tablet tools should have their own cursors
        self.cursor_status = image;
    }
}
delegate_tablet_manager!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

delegate_text_input_manager!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> InputMethodHandler for AnvilState<BackendData> {
    fn new_popup(&mut self, surface: PopupSurface) {
        if let Err(err) = self.popups.track_popup(PopupKind::from(surface)) {
            warn!("Failed to track popup: {}", err);
        }
    }

    fn popup_repositioned(&mut self, _: PopupSurface) {}

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        if let Some(parent) = surface.get_parent().map(|parent| parent.surface.clone()) {
            let _ = PopupManager::dismiss_popup(&parent, &PopupKind::from(surface));
        }
    }

    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, smithay::utils::Logical> {
        self.workspaces.space()
            .elements()
            .find_map(|window| (window.wl_surface().as_deref() == Some(parent)).then(|| window.geometry()))
            .unwrap_or_default()
    }
}

delegate_input_method_manager!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> KeyboardShortcutsInhibitHandler for AnvilState<BackendData> {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.keyboard_shortcuts_inhibit_state
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        // Just grant the wish for everyone
        inhibitor.activate();
    }
}

delegate_keyboard_shortcuts_inhibit!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

delegate_virtual_keyboard_manager!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

delegate_pointer_gestures!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

delegate_relative_pointer!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> PointerConstraintsHandler for AnvilState<BackendData> {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        // XXX region
        let Some(current_focus) = pointer.current_focus() else {
            return;
        };
        if current_focus.wl_surface().as_deref() == Some(surface) {
            with_pointer_constraint(surface, pointer, |constraint| {
                constraint.unwrap().activate();
            });
        }
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        if with_pointer_constraint(surface, pointer, |constraint| {
            constraint.is_some_and(|c| c.is_active())
        }) {
            let origin = self
                .workspaces.space()
                .elements()
                .find_map(|window: &WindowElement| {
                    (window.wl_surface().as_deref() == Some(surface)).then(|| window.geometry())
                })
                .unwrap_or_default()
                .loc
                .to_f64();

            pointer.set_location(origin + location);
        }
    }
}
delegate_pointer_constraints!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

delegate_viewporter!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> XdgActivationHandler for AnvilState<BackendData> {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        if let Some((serial, seat)) = data.serial {
            let keyboard = self.human_seat.get_keyboard().unwrap();
            Seat::from_resource(&seat) == Some(self.human_seat.clone())
                && keyboard
                    .last_enter()
                    .map(|last_enter| serial.is_no_older_than(&last_enter))
                    .unwrap_or(false)
        } else {
            false
        }
    }

    fn request_activation(
        &mut self,
        _token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if token_data.timestamp.elapsed().as_secs() < 10 {
            // Just grant the wish
            let w = self
                .workspaces.space()
                .elements()
                .find(|window: &&WindowElement| window.wl_surface().map(|s| *s == surface).unwrap_or(false))
                .cloned();
            if let Some(window) = w {
                self.workspaces.space_mut().raise_element(&window, true);
            }
        }
    }
}
delegate_xdg_activation!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> XdgDecorationHandler for AnvilState<BackendData> {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        // Default to server-side decorations — compositor draws title bar, buttons, borders
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ServerSide);
        });
    }
    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;

        toplevel.with_pending_state(|state| {
            // Honor client preference — if client explicitly requests ClientSide, allow it
            state.decoration_mode = Some(match mode {
                DecorationMode::ClientSide => Mode::ClientSide,
                _ => Mode::ServerSide,
            });
        });

        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        // When mode is unset, fall back to server-side
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ServerSide);
        });

        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}
delegate_xdg_decoration!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> KdeDecorationHandler for AnvilState<BackendData> {
    fn kde_decoration_state(&self) -> &KdeDecorationState {
        &self.kde_decoration_state
    }

    fn new_decoration(
        &mut self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        decoration: &smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::OrgKdeKwinServerDecoration,
    ) {
        use smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::Mode;
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as XdgMode;
        // Tell the client (GTK3 apps) to use server-side decorations
        decoration.mode(Mode::Server);
        // Bridge: set decoration_mode on ToplevelSurface so ack_configure evaluates is_ssd = true
        if let Some(toplevel) = self.xdg_shell_state.toplevel_surfaces().iter().find(|t| t.wl_surface() == surface) {
            toplevel.with_pending_state(|state| {
                state.decoration_mode = Some(XdgMode::ServerSide);
            });
        }
    }

    fn request_mode(
        &mut self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        decoration: &smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::OrgKdeKwinServerDecoration,
        _mode: smithay::reexports::wayland_server::WEnum<smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::Mode>,
    ) {
        use smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::Mode;
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as XdgMode;
        // Always enforce server-side decorations regardless of client request
        decoration.mode(Mode::Server);
        // Bridge: set decoration_mode on ToplevelSurface so ack_configure evaluates is_ssd = true
        if let Some(toplevel) = self.xdg_shell_state.toplevel_surfaces().iter().find(|t| t.wl_surface() == surface) {
            toplevel.with_pending_state(|state| {
                state.decoration_mode = Some(XdgMode::ServerSide);
            });
        }
    }
}
delegate_kde_decoration!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

delegate_xdg_shell!(@<BackendData: Backend + 'static> AnvilState<BackendData>);
delegate_layer_shell!(@<BackendData: Backend + 'static> AnvilState<BackendData>);
delegate_presentation!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> FractionalScaleHandler for AnvilState<BackendData> {
    fn new_fractional_scale(
        &mut self,
        surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) {
        // Here we can set the initial fractional scale
        //
        // First we look if the surface already has a primary scan-out output, if not
        // we test if the surface is a subsurface and try to use the primary scan-out output
        // of the root surface. If the root also has no primary scan-out output we just try
        // to use the first output of the toplevel.
        // If the surface is the root we also try to use the first output of the toplevel.
        //
        // If all the above tests do not lead to a output we just use the first output
        // of the space (which in case of anvil will also be the output a toplevel will
        // initially be placed on)
        #[allow(clippy::redundant_clone)]
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }

        with_states(&surface, |states| {
            let primary_scanout_output = surface_primary_scanout_output(&surface, states)
                .or_else(|| {
                    if root != surface {
                        with_states(&root, |states| {
                            surface_primary_scanout_output(&root, states).or_else(|| {
                                self.window_for_surface(&root).and_then(|window| {
                                    self.workspaces.space().outputs_for_element(&window).first().cloned()
                                })
                            })
                        })
                    } else {
                        self.window_for_surface(&root)
                            .and_then(|window| self.workspaces.space().outputs_for_element(&window).first().cloned())
                    }
                })
                .or_else(|| self.workspaces.space().outputs().next().cloned());
            if let Some(output) = primary_scanout_output {
                with_fractional_scale(states, |fractional_scale| {
                    fractional_scale.set_preferred_scale(output.current_scale().fractional_scale());
                });
            }
        });
    }
}
delegate_fractional_scale!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend + 'static> SecurityContextHandler for AnvilState<BackendData> {
    fn context_created(&mut self, source: SecurityContextListenerSource, security_context: SecurityContext) {
        self.handle
            .insert_source(source, move |client_stream, _, data| {
                let client_state = ClientState {
                    security_context: Some(security_context.clone()),
                    ..ClientState::default()
                };
                if let Err(err) = data
                    .display_handle
                    .insert_client(client_stream, Arc::new(client_state))
                {
                    warn!("Error adding wayland client: {}", err);
                };
            })
            .expect("Failed to init wayland socket source");
    }
}
delegate_security_context!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

#[cfg(feature = "xwayland")]
impl<BackendData: Backend + 'static> XWaylandKeyboardGrabHandler for AnvilState<BackendData> {
    fn keyboard_focus_for_xsurface(&self, surface: &WlSurface) -> Option<KeyboardFocusTarget> {
        let elem = self
            .workspaces.space()
            .elements()
            .find(|elem: &&WindowElement| elem.wl_surface().as_deref() == Some(surface))?;
        Some(KeyboardFocusTarget::Window(elem.0.clone()))
    }
}
#[cfg(feature = "xwayland")]
delegate_xwayland_keyboard_grab!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

#[cfg(feature = "xwayland")]
delegate_xwayland_shell!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> XdgForeignHandler for AnvilState<BackendData> {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.xdg_foreign_state
    }
}
smithay::delegate_xdg_foreign!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

smithay::delegate_single_pixel_buffer!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

smithay::delegate_fifo!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

smithay::delegate_commit_timing!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

delegate_fixes!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> ImageCaptureSourceHandler for AnvilState<BackendData> {
    fn source_destroyed(&mut self, _source: ImageCaptureSource) {
        // Anvil doesn't track sources
    }
}
smithay::delegate_image_capture_source!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> OutputCaptureSourceHandler for AnvilState<BackendData> {
    fn output_capture_source_state(&mut self) -> &mut OutputCaptureSourceState {
        &mut self.output_capture_source_state
    }

    fn output_source_created(&mut self, source: ImageCaptureSource, output: &Output) {
        source.user_data().insert_if_missing(|| output.downgrade());
    }
}
smithay::delegate_output_capture_source!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend> ImageCopyCaptureHandler for AnvilState<BackendData> {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.image_copy_capture_state
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        use smithay::output::WeakOutput;
        let weak_output = source.user_data().get::<WeakOutput>()?;
        let output = weak_output.upgrade()?;
        let mode = output.current_mode()?;

        Some(BufferConstraints {
            size: mode
                .size
                .to_logical(1)
                .to_buffer(1, smithay::utils::Transform::Normal),
            shm: vec![
                smithay::reexports::wayland_server::protocol::wl_shm::Format::Argb8888,
                smithay::reexports::wayland_server::protocol::wl_shm::Format::Xrgb8888,
            ],
            #[cfg(any(feature = "udev", feature = "winit", feature = "x11"))]
            dma: None,
        })
    }

    fn new_session(&mut self, _session: Session) {
        // Anvil doesn't track sessions; they clean up on drop
    }

    fn frame(&mut self, session: &SessionRef, frame: Frame) {
        use smithay::output::WeakOutput;

        // Resolve the session's capture source to an Output
        let source = session.source();
        let weak_output = match source.user_data().get::<WeakOutput>() {
            Some(wo) => wo,
            None => {
                frame.fail(smithay::wayland::image_copy_capture::CaptureFailureReason::Unknown);
                return;
            }
        };
        let output = match weak_output.upgrade() {
            Some(o) => o,
            None => {
                frame.fail(smithay::wayland::image_copy_capture::CaptureFailureReason::Unknown);
                return;
            }
        };

        // Check if this output belongs to an AI workspace mirror
        if let Some(workspace_id) = self.mirror.workspace_for_output(&output) {
            // Queue for rendering during the next render loop pass
            self.mirror.queue_frame(workspace_id, frame);
        } else {
            // Physical output capture — not implemented
            frame.fail(smithay::wayland::image_copy_capture::CaptureFailureReason::Unknown);
        }
    }
}
smithay::delegate_image_copy_capture!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend + 'static> AnvilState<BackendData> {
    pub fn init(
        display: Display<AnvilState<BackendData>>,
        handle: LoopHandle<'static, AnvilState<BackendData>>,
        backend_data: BackendData,
        listen_on_socket: bool,
    ) -> AnvilState<BackendData> {
        let dh = display.handle();

        let clock = Clock::new();

        // init wayland clients
        let socket_name = if listen_on_socket {
            let source = ListeningSocketSource::new_auto().unwrap();
            let socket_name = source.socket_name().to_string_lossy().into_owned();
            handle
                .insert_source(source, |client_stream, _, data| {
                    if let Err(err) = data
                        .display_handle
                        .insert_client(client_stream, Arc::new(ClientState::default()))
                    {
                        warn!("Error adding wayland client: {}", err);
                    };
                })
                .expect("Failed to init wayland socket source");
            info!(name = socket_name, "Listening on wayland socket");
            Some(socket_name)
        } else {
            None
        };
        handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, data| {
                    profiling::scope!("dispatch_clients");
                    // Safety: we don't drop the display
                    unsafe {
                        display.get_mut().dispatch_clients(data).unwrap();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .expect("Failed to init wayland server source");

        // init globals
        let compositor_state = CompositorState::new::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);
        let data_control_state =
            DataControlState::new::<Self, _>(&dh, Some(&primary_selection_state), |_| true);
        let mut seat_state = SeatState::new();
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let viewporter_state = ViewporterState::new::<Self>(&dh);
        let xdg_activation_state = XdgActivationState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let kde_decoration_state = {
            use smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration_manager::Mode as KdeDefaultMode;
            KdeDecorationState::new::<Self>(&dh, KdeDefaultMode::Server)
        };
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let presentation_state = PresentationState::new::<Self>(&dh, clock.id() as u32);
        let fractional_scale_manager_state = FractionalScaleManagerState::new::<Self>(&dh);
        let xdg_foreign_state = XdgForeignState::new::<Self>(&dh);
        let single_pixel_buffer_state = SinglePixelBufferState::new::<Self>(&dh);
        let fifo_manager_state = FifoManagerState::new::<Self>(&dh);
        let commit_timing_manager_state = CommitTimingManagerState::new::<Self>(&dh);
        TextInputManagerState::new::<Self>(&dh);
        InputMethodManagerState::new::<Self, _>(&dh, |_client| true);
        VirtualKeyboardManagerState::new::<Self, _>(&dh, |_client| true);
        // Expose global only if backend supports relative motion events
        if BackendData::HAS_RELATIVE_MOTION {
            RelativePointerManagerState::new::<Self>(&dh);
        }
        PointerConstraintsState::new::<Self>(&dh);
        if BackendData::HAS_GESTURES {
            PointerGesturesState::new::<Self>(&dh);
        }
        TabletManagerState::new::<Self>(&dh);
        SecurityContextState::new::<Self, _>(&dh, |client| {
            client
                .get_data::<ClientState>()
                .is_none_or(|client_state| client_state.security_context.is_none())
        });
        FixesState::new::<Self>(&dh);

        // Image capture protocols (screencopy)
        let image_capture_source_state = ImageCaptureSourceState::new();
        let output_capture_source_state = OutputCaptureSourceState::new::<Self>(&dh);
        let image_copy_capture_state = ImageCopyCaptureState::new::<Self>(&dh);

        // init input — human seat
        let human_seat_name = backend_data.seat_name();
        let mut human_seat = seat_state.new_wl_seat(&dh, "human".to_string());

        let human_pointer = human_seat.add_pointer();
        human_seat.add_keyboard(XkbConfig::default(), 200, 25)
            .expect("Failed to initialize the human seat keyboard");

        // init input — AI seat
        let mut ai_seat = seat_state.new_wl_seat(&dh, "ai".to_string());
        let ai_pointer = ai_seat.add_pointer();
        ai_seat.add_keyboard(XkbConfig::default(), 200, 25)
            .expect("Failed to initialize the AI seat keyboard");

        let keyboard_shortcuts_inhibit_state = KeyboardShortcutsInhibitState::new::<Self>(&dh);

        #[cfg(feature = "xwayland")]
        let xwayland_shell_state = xwayland_shell::XWaylandShellState::new::<Self>(&dh.clone());

        #[cfg(feature = "xwayland")]
        XWaylandKeyboardGrabState::new::<Self>(&dh.clone());

        // Build export state and advertise its output as a wl_output global
        // BEFORE moving dh into the struct.
        let export = {
            let mut es = crate::workspace::ExportState::new();
            let _global = es.output().create_global::<AnvilState<BackendData>>(&dh);
            es
        };

        let mut state = AnvilState {
            backend_data,
            display_handle: dh,
            socket_name,
            running: Arc::new(AtomicBool::new(true)),
            handle,
            workspaces: crate::workspace::WorkspaceManager::new(),
            mirror: crate::workspace::MirrorState::new(),
            export,
            cockpit_socket: compstr::socket::CockpitSocket::new(),
            popups: PopupManager::default(),
            compositor_state,
            data_device_state,
            layer_shell_state,
            output_manager_state,
            primary_selection_state,
            data_control_state,
            seat_state,
            keyboard_shortcuts_inhibit_state,
            shm_state,
            viewporter_state,
            xdg_activation_state,
            xdg_decoration_state,
            kde_decoration_state,
            xdg_shell_state,
            presentation_state,
            fractional_scale_manager_state,
            xdg_foreign_state,
            single_pixel_buffer_state,
            fifo_manager_state,
            commit_timing_manager_state,
            image_capture_source_state,
            output_capture_source_state,
            image_copy_capture_state,
            dnd_icon: None,
            suppressed_keys: Vec::new(),
            cursor_status: CursorImageStatus::default_named(),
            ai_cursor_status: CursorImageStatus::default_named(),
            human_seat_name,
            human_seat,
            human_pointer,
            ai_seat,
            ai_pointer,
            copilot_mode: None,
            clock,

            #[cfg(feature = "xwayland")]
            xwayland_shell_state,
            #[cfg(feature = "xwayland")]
            xwm: None,
            #[cfg(feature = "xwayland")]
            xdisplay: None,
            #[cfg(feature = "debug")]
            renderdoc: renderdoc::RenderDoc::new().ok(),
            show_window_preview: false,
            decoration_theme: DecorationTheme::load(),
            snap_preview: None,

            workspace_sockets: HashMap::new(),
        };

        // Set up IPC for workspace commands (/var/anvil/cmd/ → inotify)
        if let Err(e) = compstr::ipc::setup_ipc_watch(&state.handle) {
            warn!("Failed to set up IPC watch: {}", e);
        }

        // Drain any commands that were written before the inotify watch started
        state.process_ipc_commands();

        state
    }

    #[cfg(feature = "xwayland")]
    pub fn start_xwayland(&mut self) {
        use std::process::Stdio;

        use smithay::wayland::compositor::CompositorHandler;

        let (xwayland, client) = XWayland::spawn(
            &self.display_handle,
            None,
            std::iter::empty::<(String, String)>(),
            true,
            Stdio::null(),
            Stdio::null(),
            |_| (),
        )
        .expect("failed to start XWayland");

        let display_handle = self.display_handle.clone();
        let ret = self
            .handle
            .insert_source(xwayland, move |event, _, data| match event {
                XWaylandEvent::Ready {
                    x11_socket,
                    display_number,
                } => {
                    let xwayland_scale = std::env::var("ANVIL_XWAYLAND_SCALE")
                        .ok()
                        .and_then(|s| s.parse::<f64>().ok())
                        .unwrap_or(1.);
                    data.client_compositor_state(&client)
                        .set_client_scale(xwayland_scale);
                    let mut wm =
                        X11Wm::start_wm(data.handle.clone(), &display_handle, x11_socket, client.clone())
                            .expect("Failed to attach X11 Window Manager");

                    let cursor = Cursor::load();
                    let image = cursor.get_image(1, Duration::ZERO);
                    wm.set_cursor(
                        &image.pixels_rgba,
                        Size::from((image.width as u16, image.height as u16)),
                        Point::from((image.xhot as u16, image.yhot as u16)),
                    )
                    .expect("Failed to set xwayland default cursor");
                    data.xwm = Some(wm);
                    data.xdisplay = Some(display_number);
                }
                XWaylandEvent::Error => {
                    warn!("XWayland crashed on startup");
                }
            });
        if let Err(e) = ret {
            tracing::error!("Failed to insert the XWaylandSource into the event loop: {}", e);
        }
    }
}

impl<BackendData: Backend + 'static> AnvilState<BackendData> {
    pub fn pre_repaint(&mut self, output: &Output, frame_target: impl Into<Time<Monotonic>>) {
        let frame_target = frame_target.into();

        #[allow(clippy::mutable_key_type)]
        let mut clients: HashMap<ClientId, Client> = HashMap::new();

        // Signal commit timers for the active workspace's elements.
        // When active_workspace != 0, also signal AI workspace elements for frame callbacks.
        let active_ws = self.workspaces.active_workspace();
        let active_space = if active_ws != 0 {
            self.workspaces.get_space(active_ws)
        } else {
            None
        };
        let spaces_to_signal: Vec<&Space<crate::shell::WindowElement>> = if let Some(ai_space) = active_space {
            vec![self.workspaces.space(), ai_space]
        } else {
            vec![self.workspaces.space()]
        };
        for space in spaces_to_signal {
            space.elements().for_each(|window| {
                window.with_surfaces(|surface, states| {
                    if let Some(mut commit_timer_state) = states
                        .data_map
                        .get::<CommitTimerBarrierStateUserData>()
                        .map(|commit_timer| commit_timer.lock().unwrap())
                    {
                        commit_timer_state.signal_until(frame_target);
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    }
                });
            });
        }

        let map = smithay::desktop::layer_map_for_output(output);
        for layer_surface in map.layers() {
            layer_surface.with_surfaces(|surface, states| {
                if let Some(mut commit_timer_state) = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .map(|commit_timer| commit_timer.lock().unwrap())
                {
                    commit_timer_state.signal_until(frame_target);
                    let client = surface.client().unwrap();
                    clients.insert(client.id(), client);
                }
            });
        }
        // Drop the lock to the layer map before calling blocker_cleared, which might end up
        // calling the commit handler which in turn again could access the layer map.
        std::mem::drop(map);

        if let CursorImageStatus::Surface(ref surface) = self.cursor_status {
            with_surfaces_surface_tree(surface, |surface, states| {
                if let Some(mut commit_timer_state) = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .map(|commit_timer| commit_timer.lock().unwrap())
                {
                    commit_timer_state.signal_until(frame_target);
                    let client = surface.client().unwrap();
                    clients.insert(client.id(), client);
                }
            });
        }

        if let Some(surface) = self.dnd_icon.as_ref().map(|icon| &icon.surface) {
            with_surfaces_surface_tree(surface, |surface, states| {
                if let Some(mut commit_timer_state) = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .map(|commit_timer| commit_timer.lock().unwrap())
                {
                    commit_timer_state.signal_until(frame_target);
                    let client = surface.client().unwrap();
                    clients.insert(client.id(), client);
                }
            });
        }

        let dh = self.display_handle.clone();
        for client in clients.into_values() {
            self.client_compositor_state(&client).blocker_cleared(self, &dh);
        }
    }

    pub fn post_repaint(
        &mut self,
        output: &Output,
        time: impl Into<Duration>,
        dmabuf_feedback: Option<SurfaceDmabufFeedback>,
        render_element_states: &RenderElementStates,
    ) {
        let time = time.into();
        let throttle = Some(Duration::from_secs(1));

        #[allow(clippy::mutable_key_type)]
        let mut clients: HashMap<ClientId, Client> = HashMap::new();

        // Post-repaint for the active workspace's elements.
        // When rendering an AI workspace to the physical display, its elements need
        // frame callbacks and FIFO barrier signals too.
        let active_ws = self.workspaces.active_workspace();
        let space = if active_ws != 0 {
            self.workspaces.get_space(active_ws).unwrap_or_else(|| self.workspaces.space())
        } else {
            self.workspaces.space()
        };
        space.elements().for_each(|window| {
            window.with_surfaces(|surface, states| {
                let primary_scanout_output = surface_primary_scanout_output(surface, states);

                if let Some(output) = primary_scanout_output.as_ref() {
                    with_fractional_scale(states, |fraction_scale| {
                        fraction_scale.set_preferred_scale(output.current_scale().fractional_scale());
                    });
                }

                if primary_scanout_output
                    .as_ref()
                    .map(|o| o == output)
                    .unwrap_or(true)
                {
                    let fifo_barrier = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take();

                    if let Some(fifo_barrier) = fifo_barrier {
                        fifo_barrier.signal();
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    }
                }
            });

            if space.outputs_for_element(window).contains(output) {
                window.send_frame(output, time, throttle, surface_primary_scanout_output);
                if let Some(dmabuf_feedback) = dmabuf_feedback.as_ref() {
                    window.send_dmabuf_feedback(output, surface_primary_scanout_output, |surface, _| {
                        select_dmabuf_feedback(
                            surface,
                            render_element_states,
                            &dmabuf_feedback.render_feedback,
                            &dmabuf_feedback.scanout_feedback,
                        )
                    });
                }
            }
        });
        let map = smithay::desktop::layer_map_for_output(output);
        for layer_surface in map.layers() {
            layer_surface.with_surfaces(|surface, states| {
                let primary_scanout_output = surface_primary_scanout_output(surface, states);

                if let Some(output) = primary_scanout_output.as_ref() {
                    with_fractional_scale(states, |fraction_scale| {
                        fraction_scale.set_preferred_scale(output.current_scale().fractional_scale());
                    });
                }

                if primary_scanout_output
                    .as_ref()
                    .map(|o| o == output)
                    .unwrap_or(true)
                {
                    let fifo_barrier = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take();

                    if let Some(fifo_barrier) = fifo_barrier {
                        fifo_barrier.signal();
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    }
                }
            });

            layer_surface.send_frame(output, time, throttle, surface_primary_scanout_output);
            if let Some(dmabuf_feedback) = dmabuf_feedback.as_ref() {
                layer_surface.send_dmabuf_feedback(output, surface_primary_scanout_output, |surface, _| {
                    select_dmabuf_feedback(
                        surface,
                        render_element_states,
                        &dmabuf_feedback.render_feedback,
                        &dmabuf_feedback.scanout_feedback,
                    )
                });
            }
        }
        // Drop the lock to the layer map before calling blocker_cleared, which might end up
        // calling the commit handler which in turn again could access the layer map.
        std::mem::drop(map);

        if let CursorImageStatus::Surface(ref surface) = self.cursor_status {
            with_surfaces_surface_tree(surface, |surface, states| {
                let primary_scanout_output = surface_primary_scanout_output(surface, states);

                if let Some(output) = primary_scanout_output.as_ref() {
                    with_fractional_scale(states, |fraction_scale| {
                        fraction_scale.set_preferred_scale(output.current_scale().fractional_scale());
                    });
                }

                if primary_scanout_output
                    .as_ref()
                    .map(|o| o == output)
                    .unwrap_or(true)
                {
                    let fifo_barrier = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take();

                    if let Some(fifo_barrier) = fifo_barrier {
                        fifo_barrier.signal();
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    }
                }
            });
        }

        if let Some(surface) = self.dnd_icon.as_ref().map(|icon| &icon.surface) {
            with_surfaces_surface_tree(surface, |surface, states| {
                let primary_scanout_output = surface_primary_scanout_output(surface, states);

                if let Some(output) = primary_scanout_output.as_ref() {
                    with_fractional_scale(states, |fraction_scale| {
                        fraction_scale.set_preferred_scale(output.current_scale().fractional_scale());
                    });
                }

                if primary_scanout_output
                    .as_ref()
                    .map(|o| o == output)
                    .unwrap_or(true)
                {
                    let fifo_barrier = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take();

                    if let Some(fifo_barrier) = fifo_barrier {
                        fifo_barrier.signal();
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    }
                }
            });
        }

        let dh = self.display_handle.clone();
        for client in clients.into_values() {
            self.client_compositor_state(&client).blocker_cleared(self, &dh);
        }
    }
}

pub fn update_primary_scanout_output(
    space: &Space<WindowElement>,
    output: &Output,
    dnd_icon: &Option<DndIcon>,
    cursor_status: &CursorImageStatus,
    render_element_states: &RenderElementStates,
) {
    space.elements().for_each(|window| {
        window.with_surfaces(|surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    });
    let map = smithay::desktop::layer_map_for_output(output);
    for layer_surface in map.layers() {
        layer_surface.with_surfaces(|surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }

    if let CursorImageStatus::Surface(ref surface) = cursor_status {
        with_surfaces_surface_tree(surface, |surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }

    if let Some(surface) = dnd_icon.as_ref().map(|icon| &icon.surface) {
        with_surfaces_surface_tree(surface, |surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }
}

#[derive(Debug, Clone)]
pub struct SurfaceDmabufFeedback {
    pub render_feedback: DmabufFeedback,
    pub scanout_feedback: DmabufFeedback,
}

#[profiling::function]
pub fn take_presentation_feedback(
    output: &Output,
    space: &Space<WindowElement>,
    render_element_states: &RenderElementStates,
) -> OutputPresentationFeedback {
    let mut output_presentation_feedback = OutputPresentationFeedback::new(output);

    space.elements().for_each(|window| {
        if space.outputs_for_element(window).contains(output) {
            window.take_presentation_feedback(
                &mut output_presentation_feedback,
                surface_primary_scanout_output,
                |surface, _| surface_presentation_feedback_flags_from_states(surface, render_element_states),
            );
        }
    });
    let map = smithay::desktop::layer_map_for_output(output);
    for layer_surface in map.layers() {
        layer_surface.take_presentation_feedback(
            &mut output_presentation_feedback,
            surface_primary_scanout_output,
            |surface, _| surface_presentation_feedback_flags_from_states(surface, render_element_states),
        );
    }

    output_presentation_feedback
}

pub trait Backend {
    const HAS_RELATIVE_MOTION: bool = false;
    const HAS_GESTURES: bool = false;
    fn seat_name(&self) -> String;
    fn reset_buffers(&mut self, output: &Output);
    fn early_import(&mut self, surface: &WlSurface);
    fn update_led_state(&mut self, led_state: LedState);
}
