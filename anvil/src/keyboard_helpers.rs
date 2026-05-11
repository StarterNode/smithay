//! Anvil keyboard input synthesis — modifier parsing + XKB lookup + QWERTY table.
//!
//! Inherent impl methods on AnvilState used by compstr_ops.rs's CompositorOps
//! impl (keyboard_key / keyboard_type trait methods delegate here).
//!
//! Relocated from the deleted IpcCommand::Key and IpcCommand::Type arms in
//! anvil/src/workspace/ipc.rs during COMPSTR-AI-SEAT-LATENCY-002 phase 2.D.
//! INPUT-008 XKB QWERTY table preserved verbatim — letter keycodes follow
//! the physical QWERTY layout, NOT alphabetical order (the prior fix that
//! made 'hello world' actually produce 'hello world' instead of 'kg ''z.zv'f').

use smithay::backend::input::KeyState;
use smithay::input::keyboard::{FilterResult, Keycode};
use smithay::utils::SERIAL_COUNTER as SCOUNTER;
use tracing::warn;

use compstr::workspace::WorkspaceId;

use crate::state::{AnvilState, Backend};

impl<BackendData: Backend + 'static> AnvilState<BackendData> {
    /// Send a key press+release (with optional modifier combo) to the given
    /// workspace's AI seat keyboard. Modifiers parsed from "ctrl+l" / "alt+tab"
    /// / "shift+Return" style strings (case-insensitive).
    pub(crate) fn do_keyboard_key(&mut self, workspace: WorkspaceId, key: &str) {
        let keyboard = match self.ai_seat.get_keyboard() {
            Some(k) => k,
            None => {
                warn!("IPC: AI seat has no keyboard");
                return;
            }
        };

        // Ensure keyboard focus on target workspace window
        if let Some(space) = self.workspaces.get_space(workspace) {
            if let Some((window, _)) = space
                .element_under(self.ai_pointer.current_location())
                .map(|(w, p)| (w.clone(), p))
            {
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
                // INPUT-008: XKB keycodes follow physical QWERTY layout, NOT alphabetical order
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
                    _ => {
                        warn!("IPC: unknown key: {}", key);
                        return;
                    }
                }
            }
            _ => {
                warn!("IPC: unknown key: {}", key);
                return;
            }
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

        // Release modifiers (reverse order)
        for &mc in mod_keycodes.iter().rev() {
            keyboard.input::<(), _>(self, Keycode::new(mc), KeyState::Released, serial, 0, |_, _, _| {
                FilterResult::Forward
            });
        }
    }

    /// Send a string of characters as keyboard input to the given workspace's
    /// AI seat keyboard. Walks the string and emits per-character press+release
    /// via the XKB QWERTY lookup table (INPUT-008 fix).
    pub(crate) fn do_keyboard_type(&mut self, workspace: WorkspaceId, text: &str) {
        let keyboard = match self.ai_seat.get_keyboard() {
            Some(k) => k,
            None => {
                warn!("IPC: AI seat has no keyboard");
                return;
            }
        };

        // Ensure keyboard focus on target workspace window
        if let Some(space) = self.workspaces.get_space(workspace) {
            if let Some((window, _)) = space
                .element_under(self.ai_pointer.current_location())
                .map(|(w, p)| (w.clone(), p))
            {
                let serial = SCOUNTER.next_serial();
                keyboard.set_focus(self, Some(window.into()), serial);
            }
        }

        for ch in text.chars() {
            // INPUT-008: XKB keycodes follow physical QWERTY layout, NOT alphabetical order
            let (keycode, needs_shift) = match ch {
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
    }
}
