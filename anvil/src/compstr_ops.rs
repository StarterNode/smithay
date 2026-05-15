//! AnvilState implements compstr::ipc::CompositorOps — the seam compstr
//! drives compositor state through for INPUT IpcCommand dispatch.
//!
//! Trait method bodies are thin — 1-3 lines each, delegating to do_* helpers
//! defined here (pointer helpers) or in keyboard_helpers.rs (keyboard helpers).
//!
//! Per CEO ruling 2026-05-10:
//! - pointer_motion / pointer_button / pointer_axis are pure event emits;
//!   they do NOT call wl_pointer.frame() internally.
//! - pointer_frame is the only place wl_pointer.frame fires for pointer ops.
//! - pointer_button MAY issue an implicit motion to (x, y) before the button
//!   event to establish pointer focus (smithay PointerHandle::button uses
//!   the current pointer focus, not coordinates). handle_input does NOT need
//!   to emit pointer_motion before pointer_button.
//! - Click ships press+release+frame in ONE wl_pointer.frame (spec-strict
//!   atomic click per wayland-book pointer-input semantics).
//!
//! COMPSTR-AI-SEAT-LATENCY-002 phase 2.F.

use smithay::backend::input::{Axis, AxisSource, ButtonState};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent};
use smithay::utils::{Logical, Point, SERIAL_COUNTER as SCOUNTER};
use tracing::warn;

use compstr::ipc::CompositorOps;
use compstr::workspace::WorkspaceId;

use crate::focus::PointerFocusTarget;
use crate::state::{AnvilState, Backend};

/// Hit-test result — the focus target under the cursor + surface-local position.
/// WindowElement::surface_under returns PointerFocusTarget directly (anvil's
/// wrapper enum over WlSurface / X11Surface / SSD), so pointer.motion gets
/// the focus type it expects without conversion.
type HitTestResult = Option<(PointerFocusTarget, Point<f64, Logical>)>;

impl<BackendData: Backend + 'static> CompositorOps for AnvilState<BackendData> {
    fn get_cached_ai_workspace(&self) -> Option<WorkspaceId> {
        self.input_routing_cache.get_ai_workspace()
    }

    fn set_cached_ai_workspace(&mut self, id: WorkspaceId) {
        self.input_routing_cache.set_ai_workspace(id);
    }

    fn resolve_workspace_by_name(&self, name: &str) -> Option<WorkspaceId> {
        self.workspaces.list().into_iter().find_map(|ws| {
            (ws.name == name && self.workspaces.get_space(ws.id).is_some()).then_some(ws.id)
        })
    }

    fn pointer_motion(&mut self, workspace: WorkspaceId, x: f64, y: f64) {
        self.do_pointer_motion(workspace, x, y);
    }

    fn pointer_button(&mut self, workspace: WorkspaceId, x: f64, y: f64, button: &str, pressed: bool) {
        self.do_pointer_button(workspace, x, y, button, pressed);
    }

    fn pointer_axis(&mut self, workspace: WorkspaceId, direction: &str, amount: i32) {
        self.do_pointer_axis(workspace, direction, amount);
    }

    fn pointer_frame(&mut self, _workspace: WorkspaceId) {
        let pointer = self.ai_pointer.clone();
        pointer.frame(self);
    }

    fn keyboard_key(&mut self, workspace: WorkspaceId, key: &str) {
        self.do_keyboard_key(workspace, key);
    }

    fn keyboard_type(&mut self, workspace: WorkspaceId, text: &str) {
        self.do_keyboard_type(workspace, text);
    }
}

// Pointer helpers — the meat behind the thin trait method bodies above.
// Keyboard helpers live in keyboard_helpers.rs.
impl<BackendData: Backend + 'static> AnvilState<BackendData> {
    /// Hit-test the given workspace under cursor position pos. Returns the
    /// surface + surface-local position, or None if nothing is under.
    pub(crate) fn hit_test(&self, workspace: WorkspaceId, pos: Point<f64, Logical>) -> HitTestResult {
        self.workspaces.get_space(workspace).and_then(|space| {
            space.element_under(pos).and_then(|(window, loc)| {
                window
                    .surface_under(pos - loc.to_f64(), smithay::desktop::WindowSurfaceType::ALL)
                    .map(|(surface, surf_loc)| (surface, (surf_loc + loc).to_f64()))
            })
        })
    }

    /// Emit a pointer motion event with focus on the surface under (x, y).
    /// No frame() — caller commits via pointer_frame.
    fn do_pointer_motion(&mut self, workspace: WorkspaceId, x: f64, y: f64) {
        let pos: Point<f64, Logical> = (x, y).into();
        let pointer = self.ai_pointer.clone();
        let under = self.hit_test(workspace, pos);
        let serial = SCOUNTER.next_serial();
        pointer.motion(self, under, &MotionEvent { location: pos, serial, time: 0 });
    }

    /// Emit a pointer button event. Per CEO 2026-05-10 ruling: issues an
    /// implicit motion to (x, y) first to establish pointer focus. No frame()
    /// — caller commits via pointer_frame.
    ///
    /// Press: focus shifts to hit-test target + keyboard focus follows.
    /// Release: motion focus stays None (implicit grab routes release to
    /// press-owner) + no keyboard focus shift.
    fn do_pointer_button(
        &mut self,
        workspace: WorkspaceId,
        x: f64,
        y: f64,
        button: &str,
        pressed: bool,
    ) {
        let pos: Point<f64, Logical> = (x, y).into();
        let pointer = self.ai_pointer.clone();
        let under = if pressed { self.hit_test(workspace, pos) } else { None };
        let serial = SCOUNTER.next_serial();
        pointer.motion(self, under, &MotionEvent { location: pos, serial, time: 0 });

        let button_code: u32 = match button {
            "left" => 0x110,    // BTN_LEFT
            "right" => 0x111,   // BTN_RIGHT
            "middle" => 0x112,  // BTN_MIDDLE
            _ => 0x110,
        };
        let state = if pressed { ButtonState::Pressed } else { ButtonState::Released };
        pointer.button(self, &ButtonEvent { button: button_code, state, serial, time: 0 });

        // Press only: shift keyboard focus to the press target so subsequent
        // typing lands there. Release: no focus shift (implicit grab).
        if pressed {
            if let Some(space) = self.workspaces.get_space(workspace) {
                if let Some((window, _)) = space.element_under(pos).map(|(w, p)| (w.clone(), p)) {
                    if let Some(keyboard) = self.ai_seat.get_keyboard() {
                        keyboard.set_focus(self, Some(window.into()), serial);
                    }
                }
            }
        }
    }

    /// Emit a pointer axis (scroll) event. No frame() — caller commits via
    /// pointer_frame. Invalid direction is logged + dropped.
    fn do_pointer_axis(&mut self, _workspace: WorkspaceId, direction: &str, amount: i32) {
        let pointer = self.ai_pointer.clone();
        let multiplier = 15.0_f64;
        let value = amount as f64 * multiplier;
        let mut frame = AxisFrame::new(0).source(AxisSource::Wheel);
        match direction {
            "down" => { frame = frame.value(Axis::Vertical, value); }
            "up" => { frame = frame.value(Axis::Vertical, -value); }
            "right" => { frame = frame.value(Axis::Horizontal, value); }
            "left" => { frame = frame.value(Axis::Horizontal, -value); }
            _ => {
                warn!("IPC: invalid scroll direction: {}", direction);
                return;
            }
        }
        pointer.axis(self, frame);
    }
}
