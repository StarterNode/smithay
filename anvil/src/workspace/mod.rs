pub mod ipc;

// Re-export from compstr — all workspace/mirror/seats logic lives there now.
pub use compstr::workspace::{WorkspaceId, WorkspaceInfo, WorkspaceManager, WlSurfaceAccessor};
pub use compstr::mirror::{MirrorState, PendingFrame};
pub use compstr::seats::seat_can_view;
pub use compstr::ipc::{IpcCommand, IpcHandler};
pub use compstr::screen::export::ExportState;
pub use compstr::screen::xwayland_rootful::{XwaylandRootful, XWAYLAND_ROOTFUL_OUTPUT_NAME};

// Implement WlSurfaceAccessor for WindowElement so compstr's surface-based
// lookups work with anvil's window type.
impl WlSurfaceAccessor for crate::shell::WindowElement {
    fn wl_surface(&self) -> Option<std::borrow::Cow<'_, smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>> {
        // WindowElement already has wl_surface() — delegate to it
        self.wl_surface()
    }
}
