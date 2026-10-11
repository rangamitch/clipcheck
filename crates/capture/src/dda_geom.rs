//! Pure geometry for the no-border source (D64). No Windows API calls here so
//! every helper is unit-testable on any platform.
//! Contract: docs/impl/no-border-capture.md, section "dda_geom.rs".

/// Smallest crop (pixels, each side) worth capturing.
pub const MIN_CROP: u32 = 16;

/// An axis-aligned rectangle in pixels (right and bottom exclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub left: i32,
    /// Top edge.
    pub top: i32,
    /// Right edge (exclusive).
    pub right: i32,
    /// Bottom edge (exclusive).
    pub bottom: i32,
}

/// A crop in output-local pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropBox {
    /// Left edge.
    pub left: u32,
    /// Top edge.
    pub top: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

impl Rect {
    /// Builds a rectangle from its four edges. No validation: `right < left`
    /// is allowed and yields an empty rectangle.
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Width in pixels, computed in i64 and clamped at zero.
    pub fn width(&self) -> i64 {
        (self.right as i64 - self.left as i64).max(0)
    }

    /// Height in pixels, computed in i64 and clamped at zero.
    pub fn height(&self) -> i64 {
        (self.bottom as i64 - self.top as i64).max(0)
    }

    /// Area in square pixels. Saturates instead of overflowing for the
    /// extreme `i32` extents.
    pub fn area(&self) -> i64 {
        self.width().saturating_mul(self.height())
    }

    /// True when the rectangle has zero width or zero height.
    pub fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }

    /// Overlap of two rectangles. Returns `Rect{0,0,0,0}` when they do not
    /// overlap.
    pub fn intersect(&self, o: &Rect) -> Rect {
        let r = Rect {
            left: self.left.max(o.left),
            top: self.top.max(o.top),
            right: self.right.min(o.right),
            bottom: self.bottom.min(o.bottom),
        };
        if r.is_empty() {
            Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            }
        } else {
            r
        }
    }

    /// Moves the rectangle by `(dx, dy)`, saturating at the `i32` limits.
    pub fn translate(&self, dx: i32, dy: i32) -> Rect {
        Rect {
            left: self.left.saturating_add(dx),
            top: self.top.saturating_add(dy),
            right: self.right.saturating_add(dx),
            bottom: self.bottom.saturating_add(dy),
        }
    }
}

/// True when `window` covers at least 95 % of `monitor`'s area. False if
/// `monitor` is empty.
pub fn covers_monitor(window: Rect, monitor: Rect) -> bool {
    if monitor.is_empty() {
        return false;
    }
    let overlap = window.intersect(&monitor).area() as i128;
    let total = monitor.area() as i128;
    overlap * 100 >= total * 95
}

/// `client_screen`: window client area in virtual-screen pixels. `output_desktop`:
/// the duplicated output's DXGI DesktopCoordinates (virtual-screen pixels). Returns
/// the crop in OUTPUT-LOCAL pixels (origin = output top-left), clamped to the
/// output. None when the intersection is empty or smaller than `MIN_CROP` in
/// either dimension.
pub fn crop_box(client_screen: Rect, output_desktop: Rect) -> Option<CropBox> {
    let inter = client_screen.intersect(&output_desktop);
    if inter.is_empty() || inter.width() < MIN_CROP as i64 || inter.height() < MIN_CROP as i64 {
        return None;
    }
    // inter lies inside output_desktop, so these differences are >= 0.
    let left = inter.left as i64 - output_desktop.left as i64;
    let top = inter.top as i64 - output_desktop.top as i64;
    Some(CropBox {
        left: u32::try_from(left).ok()?,
        top: u32::try_from(top).ok()?,
        width: u32::try_from(inter.width()).ok()?,
        height: u32::try_from(inter.height()).ok()?,
    })
}

/// DXGI_MODE_ROTATION value: true for UNSPECIFIED (0) and IDENTITY (1) only.
pub fn rotation_supported(rotation: i32) -> bool {
    matches!(rotation, 0 | 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersect_disjoint_is_empty_zero_rect() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(20, 20, 30, 30);
        assert_eq!(a.intersect(&b), Rect::new(0, 0, 0, 0));
        // Touching edges share no area.
        let c = Rect::new(10, 0, 20, 10);
        assert_eq!(a.intersect(&c), Rect::new(0, 0, 0, 0));
    }

    #[test]
    fn intersect_overlap_returns_common_region() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(50, -20, 150, 60);
        assert_eq!(a.intersect(&b), Rect::new(50, 0, 100, 60));
    }

    #[test]
    fn covers_monitor_exactly_95_percent_is_true() {
        let monitor = Rect::new(0, 0, 100, 100);
        // 100 x 95 = 9500 of 10000 => exactly 95 %.
        let window = Rect::new(0, 0, 100, 95);
        assert!(covers_monitor(window, monitor));
    }

    #[test]
    fn covers_monitor_just_under_95_percent_is_false() {
        let monitor = Rect::new(0, 0, 100, 100);
        // 100 x 94 = 9400 of 10000 => 94 %.
        let window = Rect::new(0, 0, 100, 94);
        assert!(!covers_monitor(window, monitor));
    }

    #[test]
    fn covers_monitor_empty_monitor_is_false() {
        let monitor = Rect::new(5, 5, 5, 50);
        assert!(!covers_monitor(Rect::new(0, 0, 100, 100), monitor));
    }

    #[test]
    fn crop_box_negative_origin_monitor_is_output_local() {
        // Monitor sits left of and above the primary.
        let output = Rect::new(-1920, -300, 0, 780);
        let client = Rect::new(-1000, -200, -500, -50);
        let crop = crop_box(client, output).expect("crop");
        assert_eq!(
            crop,
            CropBox {
                left: 920,
                top: 100,
                width: 500,
                height: 150,
            }
        );
    }

    #[test]
    fn crop_box_clamps_window_larger_than_output() {
        let output = Rect::new(0, 0, 1920, 1080);
        let client = Rect::new(-500, -400, 2500, 1500);
        let crop = crop_box(client, output).expect("crop");
        assert_eq!(
            crop,
            CropBox {
                left: 0,
                top: 0,
                width: 1920,
                height: 1080,
            }
        );
    }

    #[test]
    fn crop_box_straddling_two_monitors_uses_this_output_only() {
        let output = Rect::new(1920, 0, 3840, 1080);
        let client = Rect::new(1500, 100, 2400, 600);
        let crop = crop_box(client, output).expect("crop");
        assert_eq!(
            crop,
            CropBox {
                left: 0,
                top: 100,
                width: 480,
                height: 500,
            }
        );
    }

    #[test]
    fn crop_box_too_small_is_none() {
        let output = Rect::new(0, 0, 1920, 1080);
        // 15 px wide: one short of MIN_CROP.
        assert!(crop_box(Rect::new(10, 10, 25, 500), output).is_none());
        // 15 px tall.
        assert!(crop_box(Rect::new(10, 10, 500, 25), output).is_none());
        // Exactly MIN_CROP is accepted.
        assert!(crop_box(Rect::new(10, 10, 26, 26), output).is_some());
    }

    #[test]
    fn crop_box_disjoint_is_none() {
        let output = Rect::new(0, 0, 1920, 1080);
        assert!(crop_box(Rect::new(2000, 0, 2500, 500), output).is_none());
    }

    #[test]
    fn extreme_coordinates_do_not_panic() {
        let full = Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
        let _ = full.area();
        let _ = full.width();
        let _ = full.intersect(&full);
        let _ = full.translate(i32::MAX, i32::MIN);
        let _ = covers_monitor(full, full);
        let _ = crop_box(full, full);
        let inverted = Rect::new(i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        assert!(inverted.is_empty());
        assert_eq!(inverted.area(), 0);
        assert!(crop_box(inverted, full).is_none());
    }

    #[test]
    fn rotation_values() {
        assert!(rotation_supported(0));
        assert!(rotation_supported(1));
        assert!(!rotation_supported(2));
        assert!(!rotation_supported(3));
    }
}
