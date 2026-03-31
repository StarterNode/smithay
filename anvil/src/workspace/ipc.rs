//! IPC command execution — anvil side.
//!
//! Types, parsing, file I/O, and inotify setup live in compstr::ipc.
//! This module implements command dispatch on AnvilState (needs compositor state).

use std::sync::Arc;

use serde_json::{json, Value};
use tracing::{error, info, warn};

use smithay::input::keyboard::{FilterResult, Keycode};
use smithay::backend::input::KeyState;
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent};
use smithay::backend::input::{Axis, AxisSource};
use smithay::utils::{Logical, Point, SERIAL_COUNTER as SCOUNTER};

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
}

impl<BackendData: Backend + 'static> AnvilState<BackendData> {
    /// COCKPIT-050-G1: Resolve target workspace for input commands.
    /// If workspace is Some, use it. Otherwise find the first AI workspace (id > 0).
    /// Falls back to workspace 1 for backwards compatibility.
    fn resolve_input_workspace(&self, workspace: Option<WorkspaceId>) -> Option<WorkspaceId> {
        if let Some(id) = workspace {
            if self.workspaces.get_space(id).is_some() {
                return Some(id);
            }
            return None;
        }
        // No workspace specified — find first AI workspace (id > 0)
        for ws in self.workspaces.list() {
            if ws.id > 0 && self.workspaces.get_space(ws.id).is_some() {
                return Some(ws.id);
            }
        }
        None
    }

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

            IpcCommand::Click { x, y, button, workspace } => {
                let ws_id = match self.resolve_input_workspace(workspace) {
                    Some(id) => id,
                    None => return ipc::error_response("no AI workspace found for click"),
                };
                let pos: Point<f64, Logical> = (x, y).into();
                let time = 0u32;

                let pointer = self.ai_pointer.clone();

                // Hit-test target workspace
                let under = self.workspaces.get_space(ws_id).and_then(|space| {
                    space.element_under(pos).and_then(|(window, loc)| {
                        window
                            .surface_under(pos - loc.to_f64(), smithay::desktop::WindowSurfaceType::ALL)
                            .map(|(surface, surf_loc)| (surface, (surf_loc + loc).to_f64()))
                    })
                });

                // Move pointer to position
                let serial = SCOUNTER.next_serial();
                pointer.motion(
                    self,
                    under,
                    &MotionEvent {
                        location: pos,
                        serial,
                        time,
                    },
                );
                pointer.frame(self);

                // Map button name to code
                let button_code: u32 = match button.as_str() {
                    "left" => 0x110,    // BTN_LEFT
                    "right" => 0x111,   // BTN_RIGHT
                    "middle" => 0x112,  // BTN_MIDDLE
                    _ => 0x110,
                };

                // Press
                let serial = SCOUNTER.next_serial();
                pointer.button(
                    self,
                    &ButtonEvent {
                        button: button_code,
                        state: smithay::backend::input::ButtonState::Pressed,
                        serial,
                        time,
                    },
                );
                pointer.frame(self);

                // Release
                let serial = SCOUNTER.next_serial();
                pointer.button(
                    self,
                    &ButtonEvent {
                        button: button_code,
                        state: smithay::backend::input::ButtonState::Released,
                        serial,
                        time: time + 50,
                    },
                );
                pointer.frame(self);

                // Set keyboard focus on the clicked window
                if let Some(space) = self.workspaces.get_space(ws_id) {
                    if let Some((window, _)) = space.element_under(pos).map(|(w, p)| (w.clone(), p)) {
                        if let Some(keyboard) = self.ai_seat.get_keyboard() {
                            keyboard.set_focus(self, Some(window.into()), serial);
                        }
                    }
                }

                info!("IPC: click at ({}, {}) button={} workspace={}", x, y, button, ws_id);
                ipc::success(json!({"action": "click", "x": x, "y": y, "button": button, "workspace": ws_id}))
            }

            IpcCommand::Move { x, y, workspace } => {
                let ws_id = match self.resolve_input_workspace(workspace) {
                    Some(id) => id,
                    None => return ipc::error_response("no AI workspace found for move"),
                };
                let pos: Point<f64, Logical> = (x, y).into();

                let pointer = self.ai_pointer.clone();

                // Hit-test target workspace
                let under = self.workspaces.get_space(ws_id).and_then(|space| {
                    space.element_under(pos).and_then(|(window, loc)| {
                        window
                            .surface_under(pos - loc.to_f64(), smithay::desktop::WindowSurfaceType::ALL)
                            .map(|(surface, surf_loc)| (surface, (surf_loc + loc).to_f64()))
                    })
                });

                let serial = SCOUNTER.next_serial();
                pointer.motion(
                    self,
                    under,
                    &MotionEvent {
                        location: pos,
                        serial,
                        time: 0,
                    },
                );
                pointer.frame(self);

                info!("IPC: move to ({}, {})", x, y);
                ipc::success(json!({"action": "move", "x": x, "y": y}))
            }

            IpcCommand::Key { key, workspace } => {
                let ws_id = match self.resolve_input_workspace(workspace) {
                    Some(id) => id,
                    None => return ipc::error_response("no AI workspace found for key"),
                };
                let keyboard = match self.ai_seat.get_keyboard() {
                    Some(k) => k,
                    None => return ipc::error_response("AI seat has no keyboard"),
                };

                // Ensure keyboard focus on target workspace window
                if let Some(space) = self.workspaces.get_space(ws_id) {
                    if let Some((window, _)) = space.element_under(
                        self.ai_pointer.current_location()
                    ).map(|(w, p)| (w.clone(), p)) {
                        let serial = SCOUNTER.next_serial();
                        keyboard.set_focus(self, Some(window.into()), serial);
                    }
                }

                // Parse modifier combo (e.g. "ctrl+l" → mods + key)
                let parts: Vec<&str> = key.split('+').collect();
                let key_name = parts.last().unwrap_or(&"");
                let mut mod_keycodes: Vec<u32> = Vec::new();

                for part in &parts[..parts.len().saturating_sub(1)] {
                    let kc = match part.to_lowercase().as_str() {
                        "ctrl" | "control" => 37, // evdev KEY_LEFTCTRL
                        "alt" => 64,              // evdev KEY_LEFTALT
                        "shift" => 50,            // evdev KEY_LEFTSHIFT
                        "super" | "win" | "meta" => 133, // evdev KEY_LEFTMETA
                        _ => continue,
                    };
                    mod_keycodes.push(kc);
                }

                // Map key name to evdev keycode
                let keycode: u32 = match key_name.to_lowercase().as_str() {
                    "return" | "enter" => 36,
                    "tab" => 23,
                    "escape" | "esc" => 9,
                    "backspace" => 22,
                    "delete" | "del" => 119,
                    "space" => 65,
                    "up" => 111, "down" => 116, "left" => 113, "right" => 114,
                    "home" => 110, "end" => 115,
                    "pageup" | "page_up" => 112, "pagedown" | "page_down" => 117,
                    "f1" => 67, "f2" => 68, "f3" => 69, "f4" => 70,
                    "f5" => 71, "f6" => 72, "f7" => 73, "f8" => 74,
                    "f9" => 75, "f10" => 76, "f11" => 95, "f12" => 96,
                    s if s.len() == 1 => {
                        let ch = s.chars().next().unwrap();
                        match ch {
                            'a' => 38, 'b' => 56, 'c' => 54, 'd' => 40, 'e' => 26,
                            'f' => 41, 'g' => 42, 'h' => 43, 'i' => 31, 'j' => 44,
                            'k' => 45, 'l' => 46, 'm' => 58, 'n' => 57, 'o' => 32,
                            'p' => 33, 'q' => 24, 'r' => 27, 's' => 39, 't' => 28,
                            'u' => 30, 'v' => 55, 'w' => 25, 'x' => 53, 'y' => 29,
                            'z' => 52,
                            '0' => 19, '1' => 10, '2' => 11, '3' => 12,
                            '4' => 13, '5' => 14, '6' => 15, '7' => 16,
                            '8' => 17, '9' => 18,
                            '/' => 61, '.' => 60, ',' => 59, ';' => 47,
                            '\'' => 48, '[' => 34, ']' => 35, '-' => 20,
                            '=' => 21, '\\' => 51, '`' => 49,
                            _ => return ipc::error_response(&format!("unknown key: {}", key)),
                        }
                    }
                    "l" => 46,
                    _ => return ipc::error_response(&format!("unknown key: {}", key)),
                };

                let serial = SCOUNTER.next_serial();

                // Press modifiers
                for &mc in &mod_keycodes {
                    keyboard.input::<(), _>(self, Keycode::new(mc), KeyState::Pressed, serial, 0, |_, _, _| {
                        FilterResult::Forward
                    });
                }

                // Press + release main key
                keyboard.input::<(), _>(self, Keycode::new(keycode), KeyState::Pressed, serial, 0, |_, _, _| {
                    FilterResult::Forward
                });
                keyboard.input::<(), _>(self, Keycode::new(keycode), KeyState::Released, serial, 0, |_, _, _| {
                    FilterResult::Forward
                });

                // Release modifiers
                for &mc in mod_keycodes.iter().rev() {
                    keyboard.input::<(), _>(self, Keycode::new(mc), KeyState::Released, serial, 0, |_, _, _| {
                        FilterResult::Forward
                    });
                }

                info!("IPC: key '{}'", key);
                ipc::success(json!({"action": "key", "key": key}))
            }

            IpcCommand::Type { text, workspace } => {
                let ws_id = match self.resolve_input_workspace(workspace) {
                    Some(id) => id,
                    None => return ipc::error_response("no AI workspace found for type"),
                };
                let keyboard = match self.ai_seat.get_keyboard() {
                    Some(k) => k,
                    None => return ipc::error_response("AI seat has no keyboard"),
                };

                // Ensure keyboard focus on target workspace window
                if let Some(space) = self.workspaces.get_space(ws_id) {
                    if let Some((window, _)) = space.element_under(
                        self.ai_pointer.current_location()
                    ).map(|(w, p)| (w.clone(), p)) {
                        let serial = SCOUNTER.next_serial();
                        keyboard.set_focus(self, Some(window.into()), serial);
                    }
                }

                for ch in text.chars() {
                    let (keycode, needs_shift) = match ch {
                        // XKB keycodes follow physical QWERTY layout, NOT alphabetical order
                        'a' => (38, false), 'b' => (56, false), 'c' => (54, false),
                        'd' => (40, false), 'e' => (26, false), 'f' => (41, false),
                        'g' => (42, false), 'h' => (43, false), 'i' => (31, false),
                        'j' => (44, false), 'k' => (45, false), 'l' => (46, false),
                        'm' => (58, false), 'n' => (57, false), 'o' => (32, false),
                        'p' => (33, false), 'q' => (24, false), 'r' => (27, false),
                        's' => (39, false), 't' => (28, false), 'u' => (30, false),
                        'v' => (55, false), 'w' => (25, false), 'x' => (53, false),
                        'y' => (29, false), 'z' => (52, false),
                        'A' => (38, true), 'B' => (56, true), 'C' => (54, true),
                        'D' => (40, true), 'E' => (26, true), 'F' => (41, true),
                        'G' => (42, true), 'H' => (43, true), 'I' => (31, true),
                        'J' => (44, true), 'K' => (45, true), 'L' => (46, true),
                        'M' => (58, true), 'N' => (57, true), 'O' => (32, true),
                        'P' => (33, true), 'Q' => (24, true), 'R' => (27, true),
                        'S' => (39, true), 'T' => (28, true), 'U' => (30, true),
                        'V' => (55, true), 'W' => (25, true), 'X' => (53, true),
                        'Y' => (29, true), 'Z' => (52, true),
                        '0' => (19, false), '1' => (10, false), '2' => (11, false),
                        '3' => (12, false), '4' => (13, false), '5' => (14, false),
                        '6' => (15, false), '7' => (16, false), '8' => (17, false),
                        '9' => (18, false),
                        ' ' => (65, false),
                        '.' => (60, false), ',' => (59, false), '/' => (61, false),
                        ';' => (47, false), '\'' => (48, false),
                        '-' => (20, false), '=' => (21, false),
                        '[' => (34, false), ']' => (35, false),
                        '\\' => (51, false), '`' => (49, false),
                        ':' => (47, true), '"' => (48, true),
                        '!' => (10, true), '@' => (11, true), '#' => (12, true),
                        '$' => (13, true), '%' => (14, true), '^' => (15, true),
                        '&' => (16, true), '*' => (17, true), '(' => (18, true),
                        ')' => (19, true), '_' => (20, true), '+' => (21, true),
                        '{' => (34, true), '}' => (35, true), '|' => (51, true),
                        '~' => (49, true), '<' => (59, true), '>' => (60, true),
                        '?' => (61, true),
                        _ => continue,
                    };

                    let serial = SCOUNTER.next_serial();

                    if needs_shift {
                        keyboard.input::<(), _>(self, Keycode::new(50), KeyState::Pressed, serial, 0, |_, _, _| {
                            FilterResult::Forward
                        });
                    }
                    keyboard.input::<(), _>(self, Keycode::new(keycode), KeyState::Pressed, serial, 0, |_, _, _| {
                        FilterResult::Forward
                    });
                    keyboard.input::<(), _>(self, Keycode::new(keycode), KeyState::Released, serial, 0, |_, _, _| {
                        FilterResult::Forward
                    });
                    if needs_shift {
                        keyboard.input::<(), _>(self, Keycode::new(50), KeyState::Released, serial, 0, |_, _, _| {
                            FilterResult::Forward
                        });
                    }
                }

                info!("IPC: type '{}' ({} chars)", text, text.len());
                ipc::success(json!({"action": "type", "text": text, "length": text.len()}))
            }

            IpcCommand::Scroll { direction, amount, workspace: _ } => {
                // workspace field accepted for API consistency but scroll targets
                // whatever surface has AI pointer focus (move pointer first — G6).
                let pointer = self.ai_pointer.clone();
                let multiplier = 15.0_f64;
                let value = amount as f64 * multiplier;

                let mut frame = AxisFrame::new(0).source(AxisSource::Wheel);
                match direction.as_str() {
                    "down" => { frame = frame.value(Axis::Vertical, value); }
                    "up" => { frame = frame.value(Axis::Vertical, -value); }
                    "right" => { frame = frame.value(Axis::Horizontal, value); }
                    "left" => { frame = frame.value(Axis::Horizontal, -value); }
                    _ => {
                        return ipc::error_response(&format!("invalid scroll direction: {}", direction));
                    }
                }

                pointer.axis(self, frame);
                pointer.frame(self);

                info!("IPC: scroll {} amount={}", direction, amount);
                ipc::success(json!({"action": "scroll", "direction": direction, "amount": amount}))
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
