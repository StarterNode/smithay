use super::HEADER_BAR_HEIGHT;
use crate::shell::grabs::ResizeEdge;

/// Regions of the SSD decoration for input routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationHitZone {
    TitleBar,
    CloseButton,
    MaximizeButton,
    BorderTop,
    BorderBottom,
    BorderLeft,
    BorderRight,
    CornerTopLeft,
    CornerTopRight,
    CornerBottomLeft,
    CornerBottomRight,
    ClientArea,
}

/// Size of corner grab zones in logical pixels.
const CORNER_SIZE: f64 = 12.0;

/// Width of edge grab zones in logical pixels (wider than visual border for usability).
const EDGE_WIDTH: f64 = 6.0;

/// Hit-test a point against SSD decoration regions.
///
/// Coordinates are window-local (0,0 is top-left of the full SSD window including title bar).
/// `window_width` is the total window width.
/// `window_height` is HEADER_BAR_HEIGHT + client_height.
/// `button_width` is the width of each title bar button.
pub fn hit_test(
    x: f64,
    y: f64,
    window_width: f64,
    window_height: f64,
    button_width: f64,
) -> DecorationHitZone {
    let header_h = HEADER_BAR_HEIGHT as f64;

    // Title bar region
    if y < header_h {
        // Top-left corner: resize zone at top-left of title bar
        if x < CORNER_SIZE && y < EDGE_WIDTH {
            return DecorationHitZone::CornerTopLeft;
        }
        // Top-right corner: resize zone at top-right of title bar
        if x > window_width - CORNER_SIZE && y < EDGE_WIDTH {
            return DecorationHitZone::CornerTopRight;
        }
        // Top edge: thin strip at the very top of title bar
        if y < EDGE_WIDTH {
            return DecorationHitZone::BorderTop;
        }
        // Close button (rightmost)
        if x >= window_width - button_width {
            return DecorationHitZone::CloseButton;
        }
        // Maximize button (second from right)
        if x >= window_width - button_width * 2.0 {
            return DecorationHitZone::MaximizeButton;
        }
        // Remaining title bar area
        return DecorationHitZone::TitleBar;
    }

    // Below title bar — check borders and corners

    // Bottom-left corner
    if x < CORNER_SIZE && y > window_height - CORNER_SIZE {
        return DecorationHitZone::CornerBottomLeft;
    }
    // Bottom-right corner
    if x > window_width - CORNER_SIZE && y > window_height - CORNER_SIZE {
        return DecorationHitZone::CornerBottomRight;
    }
    // Left edge
    if x < EDGE_WIDTH {
        return DecorationHitZone::BorderLeft;
    }
    // Right edge
    if x > window_width - EDGE_WIDTH {
        return DecorationHitZone::BorderRight;
    }
    // Bottom edge
    if y > window_height - EDGE_WIDTH {
        return DecorationHitZone::BorderBottom;
    }

    DecorationHitZone::ClientArea
}

/// Convert a decoration hit zone to a resize edge, if applicable.
pub fn resize_edge_for_zone(zone: DecorationHitZone) -> Option<ResizeEdge> {
    match zone {
        DecorationHitZone::BorderTop => Some(ResizeEdge::TOP),
        DecorationHitZone::BorderBottom => Some(ResizeEdge::BOTTOM),
        DecorationHitZone::BorderLeft => Some(ResizeEdge::LEFT),
        DecorationHitZone::BorderRight => Some(ResizeEdge::RIGHT),
        DecorationHitZone::CornerTopLeft => Some(ResizeEdge::TOP_LEFT),
        DecorationHitZone::CornerTopRight => Some(ResizeEdge::TOP_RIGHT),
        DecorationHitZone::CornerBottomLeft => Some(ResizeEdge::BOTTOM_LEFT),
        DecorationHitZone::CornerBottomRight => Some(ResizeEdge::BOTTOM_RIGHT),
        _ => None,
    }
}
