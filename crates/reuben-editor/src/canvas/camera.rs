//! The view onto the canvas: how far it is panned and zoomed, and the mapping between canvas
//! units and points on screen. Nothing here draws.

use eframe::egui::{Pos2, Rect, Vec2};

/// Far enough out to see a few hundred boxes at once.
pub const MIN_ZOOM: f32 = 0.1;

pub const MAX_ZOOM: f32 = 3.0;

/// Clear space a fitted view leaves on every side, in points.
const FIT_MARGIN: f32 = 24.0;

/// A canvas point `p` is drawn at `origin + offset + p * zoom`, where `origin` is the top-left
/// corner of the canvas's area of the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub offset: Vec2,
    pub zoom: f32,
}

impl Camera {
    pub fn to_screen(self, origin: Pos2, p: Pos2) -> Pos2 {
        origin + self.offset + p.to_vec2() * self.zoom
    }

    pub fn to_canvas(self, origin: Pos2, p: Pos2) -> Pos2 {
        ((p - origin - self.offset) / self.zoom).to_pos2()
    }

    pub fn rect_to_screen(self, origin: Pos2, r: Rect) -> Rect {
        Rect::from_min_max(self.to_screen(origin, r.min), self.to_screen(origin, r.max))
    }

    /// Move the view by `delta` points.
    pub fn pan(&mut self, delta: Vec2) {
        self.offset += delta;
    }

    /// Scale the zoom by `factor`, within its limits, keeping the canvas point under `anchor`
    /// where it is. `anchor` is measured from the canvas area's top-left corner.
    pub fn zoom_about(&mut self, anchor: Vec2, factor: f32) {
        let zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.offset = anchor - (anchor - self.offset) * (zoom / self.zoom);
        self.zoom = zoom;
    }

    /// The view that shows all of `bounds` centred in an area of `viewport` points. It zooms out
    /// as far as that takes and never in past 1:1.
    pub fn fit(bounds: Rect, viewport: Vec2) -> Self {
        let room = (viewport - Vec2::splat(2.0 * FIT_MARGIN)).max(Vec2::splat(1.0));
        let size = bounds.size().max(Vec2::splat(1.0));
        let zoom = (room.x / size.x).min(room.y / size.y).clamp(MIN_ZOOM, 1.0);
        Self {
            offset: viewport / 2.0 - bounds.center().to_vec2() * zoom,
            zoom,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: Pos2 = Pos2::new(10.0, 40.0);

    fn close(a: Pos2, b: Pos2) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn screen_and_canvas_round_trip() {
        let camera = Camera {
            offset: Vec2::new(30.0, -12.0),
            zoom: 0.5,
        };
        let p = Pos2::new(123.0, -45.0);
        assert_eq!(
            camera.to_screen(ORIGIN, p),
            Pos2::new(10.0 + 30.0 + 61.5, 40.0 - 12.0 - 22.5)
        );
        assert!(close(
            camera.to_canvas(ORIGIN, camera.to_screen(ORIGIN, p)),
            p
        ));
    }

    #[test]
    fn panning_moves_everything_by_the_drag() {
        let mut camera = Camera {
            offset: Vec2::ZERO,
            zoom: 2.0,
        };
        let before = camera.to_screen(ORIGIN, Pos2::new(5.0, 5.0));
        camera.pan(Vec2::new(7.0, -3.0));
        assert_eq!(
            camera.to_screen(ORIGIN, Pos2::new(5.0, 5.0)),
            before + Vec2::new(7.0, -3.0)
        );
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer_still() {
        let mut camera = Camera {
            offset: Vec2::new(30.0, -12.0),
            zoom: 0.5,
        };
        let anchor = Vec2::new(200.0, 150.0);
        let under = camera.to_canvas(ORIGIN, ORIGIN + anchor);
        camera.zoom_about(anchor, 1.7);
        assert!((camera.zoom - 0.85).abs() < 1e-6);
        assert!(close(camera.to_screen(ORIGIN, under), ORIGIN + anchor));
    }

    #[test]
    fn zoom_stops_at_its_limits_without_drifting() {
        let mut camera = Camera {
            offset: Vec2::new(30.0, -12.0),
            zoom: 1.0,
        };
        let anchor = Vec2::new(200.0, 150.0);
        let under = camera.to_canvas(ORIGIN, ORIGIN + anchor);
        camera.zoom_about(anchor, 1000.0);
        assert_eq!(camera.zoom, MAX_ZOOM);
        camera.zoom_about(anchor, 1e-6);
        assert_eq!(camera.zoom, MIN_ZOOM);
        let settled = camera;
        camera.zoom_about(anchor, 0.5);
        assert_eq!(camera, settled);
        assert!(close(camera.to_screen(ORIGIN, under), ORIGIN + anchor));
    }

    #[test]
    fn a_fitted_view_shows_everything_centred() {
        let bounds = Rect::from_min_max(Pos2::new(-100.0, 0.0), Pos2::new(3900.0, 1000.0));
        let viewport = Vec2::new(1000.0, 600.0);
        let camera = Camera::fit(bounds, viewport);
        let shown = camera.rect_to_screen(Pos2::ZERO, bounds);
        let area = Rect::from_min_size(Pos2::ZERO, viewport);
        assert!(area.contains_rect(shown));
        assert!(close(shown.center(), area.center()));
        assert!(camera.zoom < 1.0);
    }

    #[test]
    fn a_fitted_view_never_magnifies() {
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 40.0));
        assert_eq!(Camera::fit(bounds, Vec2::new(1000.0, 600.0)).zoom, 1.0);
    }

    #[test]
    fn a_view_too_small_to_fit_into_bottoms_out_at_the_least_zoom() {
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::splat(1e6));
        assert_eq!(Camera::fit(bounds, Vec2::ZERO).zoom, MIN_ZOOM);
    }
}
