use std::cmp::Ordering;

use objc2_core_foundation::{CGPoint, CGRect};

use crate::actor::app::WindowId;
use crate::common::config::WindowSnappingSettings;
use crate::layout_engine::Direction;
use crate::sys::geometry::CGRectExt;

// less overlap once activated for a sticky
const STICK_RATIO: f64 = 0.6;
// blend overlap and proximity into a single score (overlap still dominates).
const OVERLAP_WEIGHT: f64 = 0.7;
const CENTER_WEIGHT: f64 = 1.0 - OVERLAP_WEIGHT;
// require only a modest improvement before switching to a new candidate.
const SWITCH_DELTA: f64 = 0.04;

#[derive(Debug, Clone, Copy)]
struct CandidateMetrics {
    window: WindowId,
    overlap: f64,
    score: f64,
}

#[derive(Debug, Clone, Copy)]
struct ActiveCandidate {
    window: WindowId,
}

#[derive(Debug, Clone)]
pub struct DragManager {
    dragged_window: Option<WindowId>,
    drag_origin_frame: Option<CGRect>,
    active_candidate: Option<ActiveCandidate>,
    config: WindowSnappingSettings,
}

impl Default for DragManager {
    fn default() -> Self {
        Self::new(WindowSnappingSettings::default())
    }
}

impl DragManager {
    pub fn new(config: WindowSnappingSettings) -> Self {
        Self {
            dragged_window: None,
            drag_origin_frame: None,
            active_candidate: None,
            config,
        }
    }

    /// What dropping the dragged window on a target should do.
    ///
    /// yabai divides the target into a centre box and four edge triangles
    /// (`mouse_determine_drop_action`): the middle swaps the two windows, and
    /// each edge inserts the dragged window on that side, splitting the target.
    /// Rift only ever swapped, so there was no way to put two windows above one
    /// another on half the screen by dragging.
    pub fn drop_action(target: CGRect, cursor: CGPoint) -> DropAction {
        let w = target.size.width;
        let h = target.size.height;
        if w <= 0.0 || h <= 0.0 {
            return DropAction::Swap;
        }
        // Cursor relative to the target's top-left, so the zones below can be
        // written in plain fractions of its size.
        let p = CGPoint::new(cursor.x - target.origin.x, cursor.y - target.origin.y);

        let centre = CGRect::new(
            CGPoint::new(0.25 * w, 0.25 * h),
            objc2_core_foundation::CGSize::new(0.5 * w, 0.5 * h),
        );
        if rect_contains(centre, p) {
            return DropAction::Swap;
        }

        let mid = CGPoint::new(0.5 * w, 0.5 * h);
        let corners = [
            (CGPoint::new(0.0, 0.0), CGPoint::new(w, 0.0), Direction::Up),
            (CGPoint::new(w, 0.0), CGPoint::new(w, h), Direction::Right),
            (CGPoint::new(w, h), CGPoint::new(0.0, h), Direction::Down),
            (CGPoint::new(0.0, h), CGPoint::new(0.0, 0.0), Direction::Left),
        ];
        for (a, b, direction) in corners {
            if triangle_contains(a, mid, b, p) {
                return DropAction::Insert(direction);
            }
        }
        DropAction::Swap
    }

    /// The side of `target` the cursor is on, ignoring the centre box: the
    /// full rectangle divided into four triangles by its diagonals. For a
    /// drop that has no swap to offer (a drag from another display), every
    /// point over the target means an insert on some side, so the preview
    /// never blinks out in the middle.
    pub fn edge_direction(target: CGRect, cursor: CGPoint) -> Direction {
        let w = target.size.width.max(f64::EPSILON);
        let h = target.size.height.max(f64::EPSILON);
        let nx = (cursor.x - target.origin.x) / w - 0.5;
        let ny = (cursor.y - target.origin.y) / h - 0.5;
        if nx.abs() >= ny.abs() {
            if nx < 0.0 { Direction::Left } else { Direction::Right }
        } else if ny < 0.0 {
            Direction::Up
        } else {
            Direction::Down
        }
    }

    pub fn on_frame_change(
        &mut self,
        wid: WindowId,
        new_frame: CGRect,
        candidates: &[(WindowId, CGRect)],
    ) -> Option<WindowId> {
        self.note_dragged(wid, new_frame);

        let dragged_area = new_frame.size.width * new_frame.size.height;
        if dragged_area <= 0.0 {
            return None;
        }

        let stick_fraction = (self.config.drag_swap_fraction * STICK_RATIO)
            .clamp(0.0, self.config.drag_swap_fraction);
        let dragged_center = Self::rect_center(new_frame);
        let dragged_diag =
            f64::hypot(new_frame.size.width, new_frame.size.height).max(f64::EPSILON);

        let mut scored: Vec<CandidateMetrics> = Vec::new();
        for (other_wid, other_frame) in candidates {
            if *other_wid == wid {
                continue;
            }

            let inter = new_frame.intersection(other_frame);
            if inter.size.width <= 0.0 || inter.size.height <= 0.0 {
                continue;
            }

            let inter_area = inter.size.width * inter.size.height;

            let other_area = other_frame.size.width * other_frame.size.height;
            let union_area = dragged_area + other_area - inter_area;
            if union_area <= 0.0 {
                continue;
            }
            let iou = inter_area / union_area;
            if iou < stick_fraction {
                continue;
            }

            let other_center = Self::rect_center(*other_frame);
            let distance = f64::hypot(
                dragged_center.x - other_center.x,
                dragged_center.y - other_center.y,
            );

            let other_diag =
                f64::hypot(other_frame.size.width, other_frame.size.height).max(f64::EPSILON);
            let proximity = 1.0 - (distance / (dragged_diag + other_diag)).clamp(0.0, 1.0);
            let score = iou * OVERLAP_WEIGHT + proximity * CENTER_WEIGHT;

            scored.push(CandidateMetrics {
                window: *other_wid,
                overlap: iou,
                score,
            });
        }

        if scored.is_empty() {
            self.active_candidate = None;
            return None;
        }

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
        let best = scored[0];

        let active_metrics = self
            .active_candidate
            .and_then(|active| scored.iter().copied().find(|c| c.window == active.window));

        if let Some(active) = active_metrics {
            self.active_candidate = Some(ActiveCandidate { window: active.window });

            if active.window == best.window {
                return None;
            }

            if best.overlap >= self.config.drag_swap_fraction
                && best.score >= active.score + SWITCH_DELTA
            {
                self.active_candidate = Some(ActiveCandidate { window: best.window });
                return Some(best.window);
            }

            return None;
        }

        if best.overlap >= self.config.drag_swap_fraction {
            self.active_candidate = Some(ActiveCandidate { window: best.window });
            return Some(best.window);
        }

        self.active_candidate = None;
        None
    }

    /// Records the target outright, for a caller that has already decided
    /// which window the pointer is over. No scoring or hysteresis: the pointer
    /// is either inside a window or it is not.
    pub fn set_target(&mut self, wid: WindowId, frame: CGRect, target: Option<WindowId>) {
        self.note_dragged(wid, frame);
        self.active_candidate = target.map(|window| ActiveCandidate { window });
    }

    /// Starts tracking `wid` if it is not the window already being dragged,
    /// remembering where the drag began.
    fn note_dragged(&mut self, wid: WindowId, frame: CGRect) {
        if self.dragged_window != Some(wid) {
            self.dragged_window = Some(wid);
            self.drag_origin_frame = Some(frame);
            self.active_candidate = None;
        }
    }

    pub fn reset(&mut self) {
        self.dragged_window = None;
        self.drag_origin_frame = None;
        self.active_candidate = None;
    }

    pub fn last_target(&self) -> Option<WindowId> {
        self.active_candidate.map(|candidate| candidate.window)
    }

    pub fn dragged(&self) -> Option<WindowId> {
        self.dragged_window
    }

    pub fn origin_frame(&self) -> Option<CGRect> {
        self.drag_origin_frame
    }

    pub fn update_config(&mut self, config: WindowSnappingSettings) {
        self.config.drag_swap_fraction = if config.drag_swap_fraction <= 0.0 {
            0.5
        } else {
            config.drag_swap_fraction
        };
    }

    fn rect_center(rect: CGRect) -> CGPoint {
        CGPoint::new(
            rect.origin.x + rect.size.width * 0.5,
            rect.origin.y + rect.size.height * 0.5,
        )
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};

    use super::*;
    use crate::actor::app::WindowId;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect {
            origin: CGPoint { x, y },
            size: CGSize { width: w, height: h },
        }
    }

    #[test]
    fn drop_zones_follow_the_cursor_within_the_target() {
        use super::{DragManager, DropAction};
        use crate::layout_engine::Direction;

        let target = rect(100.0, 100.0, 400.0, 400.0);
        let at = |x: f64, y: f64| DragManager::drop_action(target, CGPoint::new(x, y));

        // The middle half in each axis swaps.
        assert_eq!(at(300.0, 300.0), DropAction::Swap);
        assert_eq!(at(210.0, 210.0), DropAction::Swap);

        // Each edge triangle inserts on that side.
        assert_eq!(at(300.0, 110.0), DropAction::Insert(Direction::Up));
        assert_eq!(at(490.0, 300.0), DropAction::Insert(Direction::Right));
        assert_eq!(at(300.0, 490.0), DropAction::Insert(Direction::Down));
        assert_eq!(at(110.0, 300.0), DropAction::Insert(Direction::Left));

        // A degenerate target cannot be divided, so it can only swap.
        assert_eq!(
            DragManager::drop_action(rect(0.0, 0.0, 0.0, 0.0), CGPoint::new(0.0, 0.0)),
            DropAction::Swap
        );
    }

    #[test]
    fn edge_direction_divides_the_whole_target_into_four_triangles() {
        use crate::layout_engine::Direction;
        let target = rect(100.0, 100.0, 400.0, 400.0);
        let at = |x: f64, y: f64| DragManager::edge_direction(target, CGPoint::new(x, y));

        assert_eq!(at(300.0, 150.0), Direction::Up);
        assert_eq!(at(450.0, 300.0), Direction::Right);
        assert_eq!(at(300.0, 450.0), Direction::Down);
        assert_eq!(at(150.0, 300.0), Direction::Left);
        // The exact middle still answers something, not nothing.
        assert_eq!(at(300.0, 300.0), Direction::Right);
    }

    #[test]
    fn selects_candidate_based_on_scored_overlap() {
        let mut dm = DragManager::new(WindowSnappingSettings { drag_swap_fraction: 0.3 });

        let dragged = rect(0.0, 0.0, 100.0, 100.0);
        let wid = WindowId::new(1, 1);

        let cand_a = (WindowId::new(1, 2), rect(0.0, 0.0, 40.0, 100.0)); // 40%
        let cand_b = (WindowId::new(1, 3), rect(0.0, 0.0, 60.0, 100.0)); // 60%

        let chosen = dm.on_frame_change(wid, dragged, &[cand_a, cand_b]);
        assert_eq!(chosen, Some(WindowId::new(1, 3)));
    }

    #[test]
    fn respects_last_target_to_avoid_repeats() {
        let mut dm = DragManager::new(WindowSnappingSettings { drag_swap_fraction: 0.25 });
        let wid = WindowId::new(1, 10);
        let dragged = rect(0.0, 0.0, 200.0, 100.0);

        let cand = (WindowId::new(1, 20), rect(0.0, 0.0, 100.0, 100.0)); // 50% overlap

        let chosen1 = dm.on_frame_change(wid, dragged, &[cand]);
        assert_eq!(chosen1, Some(WindowId::new(1, 20)));

        let chosen2 = dm.on_frame_change(wid, dragged, &[cand]);
        assert_eq!(chosen2, None);
    }

    #[test]
    fn clears_active_target_when_overlap_is_lost() {
        let mut dm = DragManager::new(WindowSnappingSettings { drag_swap_fraction: 0.2 });
        let wid = WindowId::new(1, 42);
        let dragged = rect(0.0, 0.0, 100.0, 100.0);
        let cand = (WindowId::new(1, 99), rect(0.0, 0.0, 60.0, 100.0));

        let chosen = dm.on_frame_change(wid, dragged, &[cand]);
        assert_eq!(chosen, Some(WindowId::new(1, 99)));
        assert_eq!(dm.last_target(), Some(WindowId::new(1, 99)));

        let moved = rect(200.0, 0.0, 100.0, 100.0);
        let cleared = dm.on_frame_change(wid, moved, &[cand]);
        assert!(cleared.is_none());
        assert!(dm.last_target().is_none());
    }

    #[test]
    fn hysteresis_keeps_candidate_when_overlap_drops_slightly() {
        let mut dm = DragManager::new(WindowSnappingSettings { drag_swap_fraction: 0.4 });
        let wid = WindowId::new(5, 1);
        let dragged = rect(0.0, 0.0, 100.0, 100.0);
        let cand = (WindowId::new(5, 2), rect(0.0, 0.0, 50.0, 100.0)); // 50%

        let chosen = dm.on_frame_change(wid, dragged, &[cand]);
        assert_eq!(chosen, Some(WindowId::new(5, 2)));

        let shifted = rect(20.0, 0.0, 100.0, 100.0); // 30% overlap
        let result = dm.on_frame_change(wid, shifted, &[cand]);
        assert!(result.is_none());
        assert_eq!(dm.last_target(), Some(WindowId::new(5, 2)));
    }

    #[test]
    fn switches_only_when_new_candidate_is_meaningfully_better() {
        let mut dm = DragManager::new(WindowSnappingSettings { drag_swap_fraction: 0.3 });
        let wid = WindowId::new(7, 1);
        let dragged = rect(0.0, 0.0, 120.0, 100.0);

        let cand_a = (WindowId::new(7, 2), rect(0.0, 0.0, 60.0, 100.0)); // 50%
        let cand_b = (WindowId::new(7, 3), rect(0.0, 0.0, 68.0, 100.0)); // 56.6%

        assert_eq!(
            dm.on_frame_change(wid, dragged, &[cand_a, cand_b]),
            Some(WindowId::new(7, 3))
        );

        let cand_a_shifted = (WindowId::new(7, 2), rect(0.0, 0.0, 66.0, 100.0)); // 55%
        let result = dm.on_frame_change(wid, dragged, &[cand_a_shifted, cand_b]);
        assert!(result.is_none());
        assert_eq!(dm.last_target(), Some(WindowId::new(7, 3)));

        let cand_a_dominant = (WindowId::new(7, 2), rect(-10.0, 0.0, 120.0, 100.0)); // 100% overlap
        let switched = dm.on_frame_change(wid, dragged, &[cand_a_dominant, cand_b]);
        assert_eq!(switched, Some(WindowId::new(7, 2)));
        assert_eq!(dm.last_target(), Some(WindowId::new(7, 2)));
    }
}

/// What a drop on a target window means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropAction {
    /// Exchange the two windows' places.
    Swap,
    /// Split the target and put the dragged window on that side of it.
    Insert(Direction),
}

fn rect_contains(rect: CGRect, point: CGPoint) -> bool {
    point.x >= rect.origin.x
        && point.y >= rect.origin.y
        && point.x <= rect.origin.x + rect.size.width
        && point.y <= rect.origin.y + rect.size.height
}

/// Whether `p` lies inside triangle `abc`, by consistent winding of the three
/// edge cross products.
fn triangle_contains(a: CGPoint, b: CGPoint, c: CGPoint, p: CGPoint) -> bool {
    let cross =
        |u: CGPoint, v: CGPoint, w: CGPoint| (v.x - u.x) * (w.y - u.y) - (v.y - u.y) * (w.x - u.x);
    let d1 = cross(a, b, p);
    let d2 = cross(b, c, p);
    let d3 = cross(c, a, p);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}
