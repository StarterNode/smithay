//! Button icon geometry for SSD title bar.
//! Pure path definitions — no rendering logic.

use tiny_skia::{Path, PathBuilder};

/// Close button icon: X cross centered at (cx, cy) with given half-size.
pub fn close_icon_path(cx: f32, cy: f32, half_size: f32) -> Option<Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(cx - half_size, cy - half_size);
    pb.line_to(cx + half_size, cy + half_size);
    pb.move_to(cx + half_size, cy - half_size);
    pb.line_to(cx - half_size, cy + half_size);
    pb.finish()
}

/// Maximize button icon: square outline centered at (cx, cy) with given half-size.
pub fn maximize_icon_path(cx: f32, cy: f32, half_size: f32) -> Option<Path> {
    let mut pb = PathBuilder::new();
    let x = cx - half_size;
    let y = cy - half_size;
    let s = half_size * 2.0;
    pb.move_to(x, y);
    pb.line_to(x + s, y);
    pb.line_to(x + s, y + s);
    pb.line_to(x, y + s);
    pb.close();
    pb.finish()
}
