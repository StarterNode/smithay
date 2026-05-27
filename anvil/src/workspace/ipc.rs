//! IPC command execution — anvil side.
//!
//! Types, parsing, file I/O, and inotify setup live in compstr::ipc.
//! INPUT-variant dispatch (Click, MouseDown, MouseUp, Move, Scroll, Key, Type)
//! moved to compstr::ipc::dispatch::handle_input in COMPSTR-AI-SEAT-LATENCY-002
//! phase 2.D — anvil only retains non-INPUT command execution here (workspace
//! lifecycle: Create / Destroy / Subscribe / Unsubscribe / List / Switch / Spawn).
//! The IpcHandler::dispatch_input_ipc_command override + file-IPC INPUT-arm
//! delegation to handle_input land in phase 2.G.

use std::sync::Arc;

use serde_json::{json, Value};
use tracing::{error, info, warn};

use compstr::ipc::{self, IpcCommand, IpcHandler};
use compstr::workspace::WorkspaceId;
use crate::state::{AnvilState, Backend, ClientState};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

/// XPRA-008 bridge — locate the chromium kiosk wl_surface on the given workspace.
/// Walks the workspace's space.elements() and matches xdg_toplevel.app_id by either
/// the strict is_aidesktop matcher (manji.aidesktop / chrome-127.0.0.1__-Default)
/// OR the loose chrome-127.0.0.1__*-Default pattern (chromium's URL-app-mode form
/// for any 127.0.0.1 URL including /ai_desktop, /drive, etc.) OR ANY toplevel on
/// the workspace as a last-ditch fallback (the AI workspace only ever has the one
/// kiosk by design). Logs each candidate's app_id for diagnostics — DEMO-DAY-002.
fn find_kiosk_surface<B: Backend + 'static>(
    state: &AnvilState<B>,
    workspace_id: WorkspaceId,
) -> Option<WlSurface> {
    let space = state.workspaces.get_space(workspace_id)?;
    let mut first_fallback: Option<WlSurface> = None;
    let mut chromium_fallback: Option<WlSurface> = None;
    for elem in space.elements() {
        let Some(surface) = elem.wl_surface() else { continue };
        let app_id = with_states(&surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|d| d.app_id.clone()))
        }).unwrap_or_default();
        tracing::info!("XPRA-008 BRIDGE candidate: app_id={:?}", app_id);
        if compstr::system_apps::is_aidesktop(&app_id) {
            return Some(surface.into_owned());
        }
        // Chromium URL-app-mode forms: chrome-127.0.0.1*Default etc.
        if app_id.starts_with("chrome-127.0.0.1") && app_id.ends_with("-Default") {
            chromium_fallback.get_or_insert_with(|| surface.clone().into_owned());
        }
        first_fallback.get_or_insert_with(|| surface.clone().into_owned());
    }
    chromium_fallback.or(first_fallback)
}

/// AnvilState implements IpcHandler so compstr::ipc::setup_ipc_watch can
/// call back into the compositor without knowing about AnvilState internals.
impl<BackendData: Backend + 'static> IpcHandler for AnvilState<BackendData> {
    fn process_ipc_commands(&mut self) {
        let commands = ipc::scan_commands();

        for (uuid, cmd) in commands {
            let response = self.handle_ipc_command(cmd);

            if let Err(e) = ipc::write_response(&uuid, &response) {
                error!("IPC: failed to write response for {}: {}", uuid, e);
            }
        }
    }

    /// COMPSTR-AI-SEAT-LATENCY-002 phase 2.G: socket-transport INPUT dispatch
    /// delegates to compstr's owned handler. One-line thin hook per CEO ruling
    /// 2026-05-10 — compstr owns the dispatch logic; anvil owns CompositorOps.
    fn dispatch_input_ipc_command(&mut self, cmd: IpcCommand) {
        compstr::ipc::handle_input(self, cmd);
    }
}

impl<BackendData: Backend + 'static> AnvilState<BackendData> {
    fn handle_ipc_command(&mut self, cmd: IpcCommand) -> Value {
        match cmd {
            IpcCommand::Create { name } => {
                let id = self.workspaces.create(&name);
                // Map export output to AI workspace so clients can fullscreen to it
                if id > 0 {
                    if let Some(space) = self.workspaces.get_space_mut(id) {
                        space.map_output(self.export.output(), (0, 0));
                    }
                }
                info!("IPC: created workspace '{}' (id={})", name, id);
                ipc::success(json!({"id": id, "name": name}))
            }

            IpcCommand::Destroy { id } => {
                if id == 0 {
                    return ipc::error_response("cannot destroy workspace 0 (desktop)");
                }
                // Clean up mirror subscription
                if let Some(global_id) = self.mirror.workspace_destroyed(id) {
                    self.display_handle.remove_global::<Self>(global_id);
                }
                // Remove workspace socket if any
                self.workspace_sockets.remove(&id);
                // Destroy the workspace
                if self.workspaces.destroy(id) {
                    info!("IPC: destroyed workspace {}", id);
                    ipc::success(json!({"id": id}))
                } else {
                    ipc::error_response(&format!("workspace {} not found", id))
                }
            }

            IpcCommand::Subscribe { id } => {
                if self.workspaces.get_space(id).is_none() {
                    return ipc::error_response(&format!("workspace {} not found", id));
                }
                if self.mirror.is_subscribed(id) {
                    return ipc::error_response(&format!("workspace {} already subscribed", id));
                }

                // Derive dimensions from physical output mode (workspace 0's display).
                let (width, height) = self.workspaces.space().outputs().next()
                    .and_then(|o| o.current_mode())
                    .map(|m| (m.size.w, m.size.h))
                    .unwrap_or((1920, 1080));

                // Subscribe creates a virtual Output; register it as a Wayland global
                let global_id = {
                    let output = self.mirror.subscribe(id, width, height);
                    output.create_global::<Self>(&self.display_handle)
                };
                self.mirror.set_global_id(id, global_id);

                info!("IPC: subscribed to workspace {}", id);
                ipc::success(json!({"id": id}))
            }

            IpcCommand::Unsubscribe { id } => {
                if let Some(global_id) = self.mirror.unsubscribe(id) {
                    self.display_handle.remove_global::<Self>(global_id);
                    info!("IPC: unsubscribed from workspace {}", id);
                    ipc::success(json!({"id": id}))
                } else {
                    ipc::error_response(&format!("workspace {} not subscribed", id))
                }
            }

            IpcCommand::List => {
                let list = self.workspaces.list();
                ipc::success(json!(list))
            }

            IpcCommand::EngagePeacock { id } => {
                // XPRA-008 bridge architecture (2026-05-27 late evening pivot, supersedes
                // the prior output-reassignment ship at ca1b596). On engage we look up
                // the chromium kiosk wl_surface on ws_ai by app_id == manji.aidesktop
                // (compstr::system_apps::is_aidesktop) and store it on AnvilState so the
                // input_handler can short-circuit human pointer + keyboard events
                // directly to it when the pointer is over cpit's mirror rect. Keeps
                // ws_0 and ws_ai fully isolated — only the input plane crosses the gap.
                if self.workspaces.get_space(id).is_none() {
                    return ipc::error_response(&format!("workspace {} not found", id));
                }
                self.drive_mode = Some(id);

                let kiosk_surface = find_kiosk_surface(self, id);
                self.drive_mode_target = kiosk_surface.clone();

                // Force keyboard focus to the kiosk for the duration of drive mode so
                // keystrokes flow to chromium DOM. Set focus on BOTH human and ai
                // seat — kiosk client on ws_ai only sees the ai seat per the
                // compstr two-seat filter (compstr::seats::seat_can_view), so
                // human-seat focus alone wouldn't reach chromium. Restored on
                // Disengage.
                if let Some(kiosk) = kiosk_surface.as_ref() {
                    let window = self.workspaces.window_for_surface(kiosk);
                    let kbd_focus = window.map(crate::focus::KeyboardFocusTarget::from);
                    let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                    if let Some(kb) = self.human_seat.get_keyboard() {
                        kb.set_focus(self, kbd_focus.clone(), serial);
                    }
                    if let Some(kb) = self.ai_seat.get_keyboard() {
                        kb.set_focus(self, kbd_focus, serial);
                    }
                }

                info!(
                    "IPC: drive engaged — workspace {} (drive_mode_target = {})",
                    id,
                    if self.drive_mode_target.is_some() { "Some(kiosk)" } else { "None — kiosk surface not found" }
                );
                ipc::success(json!({
                    "id": id,
                    "drive": true,
                    "bridge_armed": self.drive_mode_target.is_some()
                }))
            }

            IpcCommand::DisengagePeacock => {
                // XPRA-008 bridge — release the kiosk surface + restore keyboard focus
                // to standard surface_under dispatch.
                self.drive_mode = None;
                let was_armed = self.drive_mode_target.is_some();
                self.drive_mode_target = None;
                let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                if let Some(kb) = self.human_seat.get_keyboard() {
                    kb.set_focus(self, None, serial);
                }
                if let Some(kb) = self.ai_seat.get_keyboard() {
                    kb.set_focus(self, None, serial);
                }
                info!(
                    "IPC: drive disengaged — bridge released (was armed: {})",
                    was_armed
                );
                ipc::success(json!({"drive": false, "bridge_released": was_armed}))
            }

            IpcCommand::Spawn { id, command, args } => {
                if self.workspaces.get_space(id).is_none() {
                    return ipc::error_response(&format!("workspace {} not found", id));
                }

                let socket_name = self.ensure_workspace_socket(id);

                match std::process::Command::new(&command)
                    .args(&args)
                    .env("WAYLAND_DISPLAY", &socket_name)
                    .spawn()
                {
                    Ok(child) => {
                        let pid = child.id();
                        info!(
                            "IPC: spawned '{}' (pid={}) into workspace {} via socket {}",
                            command, pid, id, socket_name
                        );
                        ipc::success(json!({
                            "pid": pid,
                            "workspace_id": id,
                            "socket": socket_name
                        }))
                    }
                    Err(e) => {
                        error!("IPC: failed to spawn '{}': {}", command, e);
                        ipc::error_response(&format!("spawn failed: {}", e))
                    }
                }
            }

            // COMPSTR-AI-SEAT-LATENCY-002 phase 2.G: file-IPC INPUT arrivals
            // (cpit drive emit_* + kore mouse/scroll during the migration window)
            // delegate to compstr's owned dispatcher. write_response handles the
            // nowait-UUID skip (QW2 receiver) for cpit's fire-and-forget tagging;
            // kore CLI callers without the prefix receive the success ack.
            cmd @ (IpcCommand::Click { .. }
                 | IpcCommand::MouseDown { .. }
                 | IpcCommand::MouseUp { .. }
                 | IpcCommand::Move { .. }
                 | IpcCommand::Scroll { .. }
                 | IpcCommand::Key { .. }
                 | IpcCommand::Type { .. }) => {
                compstr::ipc::handle_input(self, cmd);
                ipc::success(json!({"action": "input", "transport": "file-ipc"}))
            }
        }
    }

    /// Ensure a workspace-specific Wayland socket exists. Creates one if needed.
    pub fn ensure_workspace_socket(&mut self, id: WorkspaceId) -> String {
        if let Some(name) = self.workspace_sockets.get(&id) {
            return name.clone();
        }

        let socket_name = format!("wayland-ws-{}", id);
        match smithay::wayland::socket::ListeningSocketSource::with_name(&socket_name) {
            Ok(source) => {
                let ws_id = id;
                self.handle
                    .insert_source(source, move |client_stream, _, data| {
                        let client_state = ClientState {
                            workspace_id: std::sync::Mutex::new(Some(ws_id)),
                            ..ClientState::default()
                        };
                        if let Err(err) = data
                            .display_handle
                            .insert_client(client_stream, Arc::new(client_state))
                        {
                            warn!("Error adding workspace {} client: {}", ws_id, err);
                        }
                    })
                    .expect("Failed to init workspace socket");

                info!(
                    "IPC: created workspace socket '{}' for workspace {}",
                    socket_name, id
                );
                self.workspace_sockets.insert(id, socket_name.clone());
                socket_name
            }
            Err(e) => {
                error!(
                    "IPC: failed to create socket '{}': {}. Falling back to default socket.",
                    socket_name, e
                );
                self.socket_name.clone().unwrap_or_default()
            }
        }
    }
}
