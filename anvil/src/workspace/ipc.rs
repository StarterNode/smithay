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

            IpcCommand::Switch { id } => {
                if id == 0 {
                    self.workspaces.set_active_workspace(0);
                    self.copilot_mode = None;
                    info!("IPC: switched to desktop (copilot off)");
                    ipc::success(json!({"id": 0, "copilot": false}))
                } else if self.workspaces.get_space(id).is_none() {
                    ipc::error_response(&format!("workspace {} not found", id))
                } else {
                    self.workspaces.set_active_workspace(id);
                    self.copilot_mode = Some(id);
                    info!("IPC: switched to workspace {} (copilot on)", id);
                    ipc::success(json!({"id": id, "copilot": true}))
                }
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
