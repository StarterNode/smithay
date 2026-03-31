use smithay::{
    backend::renderer::element::solid::SolidColorBuffer,
    desktop::layer_map_for_output,
    output::Output,
    utils::{Logical, Point, Rectangle},
};

use super::ssd::{DecorationTheme, HEADER_BAR_HEIGHT};

/// A detected snap zone during window move grab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapZone {
    LeftHalf,
    RightHalf,
    Maximize,
    TopLeftQuarter,
    TopRightQuarter,
    BottomLeftQuarter,
    BottomRightQuarter,
}

/// Active snap preview state, stored on AnvilState during a move grab.
#[derive(Debug)]
pub struct SnapPreview {
    pub zone: SnapZone,
    pub rect: Rectangle<i32, Logical>,
    pub buffer: SolidColorBuffer,
    pub opacity: f32,
}

/// Get the usable area for an output (output geometry minus exclusive zones from cockpit/dock).
pub fn usable_area(
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let map = layer_map_for_output(output);
    let zone = map.non_exclusive_zone();
    Rectangle::new(
        (output_geo.loc.x + zone.loc.x, output_geo.loc.y + zone.loc.y).into(),
        zone.size,
    )
}

/// Detect which snap zone (if any) the pointer is in.
/// Returns None if pointer is not near any edge.
pub fn detect_snap_zone(
    pointer: Point<f64, Logical>,
    usable: Rectangle<i32, Logical>,
    trigger_distance: i32,
) -> Option<SnapZone> {
    let px = pointer.x as i32;
    let py = pointer.y as i32;

    let left = usable.loc.x;
    let top = usable.loc.y;
    let right = usable.loc.x + usable.size.w;
    let bottom = usable.loc.y + usable.size.h;

    let near_left = px <= left + trigger_distance;
    let near_right = px >= right - trigger_distance;
    let near_top = py <= top + trigger_distance;
    let near_bottom = py >= bottom - trigger_distance;

    // Corners take priority over edges
    if near_top && near_left {
        Some(SnapZone::TopLeftQuarter)
    } else if near_top && near_right {
        Some(SnapZone::TopRightQuarter)
    } else if near_bottom && near_left {
        Some(SnapZone::BottomLeftQuarter)
    } else if near_bottom && near_right {
        Some(SnapZone::BottomRightQuarter)
    } else if near_left {
        Some(SnapZone::LeftHalf)
    } else if near_right {
        Some(SnapZone::RightHalf)
    } else if near_top {
        Some(SnapZone::Maximize)
    } else {
        None
    }
}

/// Compute the geometry rectangle for a snap zone within the usable area.
pub fn snap_zone_geometry(
    zone: SnapZone,
    usable: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let x = usable.loc.x;
    let y = usable.loc.y;
    let w = usable.size.w;
    let h = usable.size.h;
    let hw = w / 2;
    let hh = h / 2;

    match zone {
        SnapZone::LeftHalf => Rectangle::new((x, y).into(), (hw, h).into()),
        SnapZone::RightHalf => Rectangle::new((x + hw, y).into(), (w - hw, h).into()),
        SnapZone::Maximize => Rectangle::new((x, y).into(), (w, h).into()),
        SnapZone::TopLeftQuarter => Rectangle::new((x, y).into(), (hw, hh).into()),
        SnapZone::TopRightQuarter => Rectangle::new((x + hw, y).into(), (w - hw, hh).into()),
        SnapZone::BottomLeftQuarter => Rectangle::new((x, y + hh).into(), (hw, h - hh).into()),
        SnapZone::BottomRightQuarter => {
            Rectangle::new((x + hw, y + hh).into(), (w - hw, h - hh).into())
        }
    }
}

/// Compute the client configure size for a snapped SSD window.
/// Subtracts HEADER_BAR_HEIGHT from height for title bar.
pub fn snap_client_size(
    zone_rect: Rectangle<i32, Logical>,
    is_ssd: bool,
) -> (i32, i32) {
    let w = zone_rect.size.w;
    let h = if is_ssd {
        zone_rect.size.h - HEADER_BAR_HEIGHT
    } else {
        zone_rect.size.h
    };
    (w, h)
}

impl SnapPreview {
    /// Create a new snap preview from a detected zone and theme.
    pub fn new(
        zone: SnapZone,
        usable: Rectangle<i32, Logical>,
        theme: &DecorationTheme,
    ) -> Self {
        let rect = snap_zone_geometry(zone, usable);
        let mut color = theme.snap_preview.color;
        // Apply opacity to the color alpha channel
        color[3] = theme.snap_preview.opacity;
        let mut buffer = SolidColorBuffer::default();
        buffer.update((rect.size.w, rect.size.h), color);
        Self {
            zone,
            rect,
            buffer,
            opacity: 1.0, // alpha already baked into the color
        }
    }
}
