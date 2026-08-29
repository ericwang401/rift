//! The region a dragged window would land in, drawn over the screen.
//!
//! yabai draws this by creating a SkyLight window per tree node and stroking a
//! flat rectangle into it with CGContext, then destroying the window when the
//! target changes — so the feedback blinks from place to place and has hard
//! square edges (`insert_feedback_show` in its view.c).
//!
//! This keeps one window per display and moves a single rounded layer inside
//! it, over a blurred backing the window server composites for us. Because the
//! layer is rendered as a snapshot rather than by Core Animation, motion is not
//! free: the caller advances `step` on a timer and the layer eases toward its
//! target, which is what makes the region glide between drop zones instead of
//! jumping.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2_app_kit::NSStatusWindowLevel;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_quartz_core::CALayer;
use tracing::warn;

use crate::sys::cgs_window::{CgsWindow, CgsWindowError};
use crate::ui::common::{render_layer_to_cgs_window, with_disabled_actions};
use crate::ui::stack_line::Color;

/// How close the drawn region has to get to its target before the animation is
/// considered finished, in points. Below a pixel there is nothing left to see.
const SETTLE_EPSILON: f64 = 0.5;

#[derive(Debug, Clone, Copy)]
pub struct DropOverlayConfig {
    pub fill: Color,
    pub border: Color,
    pub border_width: f64,
    pub corner_radius: f64,
    /// Blur radius for the backing. Zero leaves it unblurred.
    pub blur_radius: i32,
    /// Fraction of the remaining distance covered per frame, before easing.
    /// Higher is snappier; 1.0 removes the animation entirely.
    pub follow_rate: f64,
}

impl Default for DropOverlayConfig {
    fn default() -> Self {
        Self {
            fill: Color::new(0.0, 0.48, 1.0, 0.20),
            border: Color::new(0.0, 0.55, 1.0, 0.90),
            border_width: 2.0,
            corner_radius: 10.0,
            blur_radius: 24,
            follow_rate: 0.35,
        }
    }
}

pub struct DropOverlayWindow {
    screen: CGRect,
    config: DropOverlayConfig,
    cgs_window: CgsWindow,
    root_layer: Retained<CALayer>,
    highlight: Retained<CALayer>,
    /// Where the highlight is drawn now, and where it is heading, both in
    /// window-local coordinates.
    drawn: RefCell<Option<CGRect>>,
    target: RefCell<Option<CGRect>>,
}

impl DropOverlayWindow {
    pub fn new(screen: CGRect, config: DropOverlayConfig) -> Result<Self, CgsWindowError> {
        let root_layer = CALayer::layer();
        root_layer.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), screen.size));

        let highlight = CALayer::layer();
        let cgs_window = CgsWindow::new(screen)?;
        cgs_window.set_opacity(false)?;
        cgs_window.set_alpha(0.0)?;
        // Above ordinary windows: this describes where a window is going, so it
        // has to be legible over the ones already there.
        cgs_window.set_level(NSStatusWindowLevel as i32)?;
        // Bit 3 disables the system drop shadow, which would otherwise outline
        // a translucent overlay with a hard rectangle.
        cgs_window.set_tags(1 << 3)?;
        if config.blur_radius > 0 {
            if let Err(error) = cgs_window.set_blur(config.blur_radius, None) {
                warn!(?error, "drop overlay could not enable background blur");
            }
        }

        let overlay = Self {
            screen,
            config,
            cgs_window,
            root_layer,
            highlight,
            drawn: RefCell::new(None),
            target: RefCell::new(None),
        };
        overlay.style_highlight();
        overlay.root_layer.addSublayer(&overlay.highlight);
        Ok(overlay)
    }

    pub fn screen(&self) -> CGRect {
        self.screen
    }

    fn style_highlight(&self) {
        with_disabled_actions(|| unsafe {
            self.highlight.setCornerRadius(self.config.corner_radius);
            self.highlight.setBorderWidth(self.config.border_width);
            self.highlight
                .setBackgroundColor(Some(&self.config.fill.to_nscolor().CGColor()));
            self.highlight.setBorderColor(Some(&self.config.border.to_nscolor().CGColor()));
        });
    }

    /// Points the overlay at a region, in global screen coordinates.
    ///
    /// The first region appears where it is asked for; later ones are eased
    /// toward, so moving between drop zones reads as one region moving rather
    /// than two regions blinking.
    pub fn aim_at(&self, region: CGRect) {
        let local = CGRect::new(
            CGPoint::new(
                region.origin.x - self.screen.origin.x,
                region.origin.y - self.screen.origin.y,
            ),
            region.size,
        );
        *self.target.borrow_mut() = Some(local);
        if self.drawn.borrow().is_none() {
            *self.drawn.borrow_mut() = Some(local);
        }
        self.present();
    }

    /// Advances the animation one frame. Returns whether more frames are
    /// needed, so the caller can stop its timer once the region has settled.
    pub fn step(&self) -> bool {
        let Some(target) = *self.target.borrow() else {
            return false;
        };
        let Some(drawn) = *self.drawn.borrow() else {
            return false;
        };
        if rects_close(drawn, target) {
            *self.drawn.borrow_mut() = Some(target);
            self.present();
            return false;
        }
        let next = approach(drawn, target, self.config.follow_rate);
        *self.drawn.borrow_mut() = Some(next);
        self.present();
        true
    }

    pub fn hide(&self) {
        *self.target.borrow_mut() = None;
        *self.drawn.borrow_mut() = None;
        if let Err(error) = self.cgs_window.set_alpha(0.0) {
            warn!(?error, "drop overlay could not hide");
        }
        if let Err(error) = self.cgs_window.order_out() {
            warn!(?error, "drop overlay could not order out");
        }
    }

    fn present(&self) {
        let Some(frame) = *self.drawn.borrow() else {
            return;
        };
        with_disabled_actions(|| self.highlight.setFrame(frame));
        render_layer_to_cgs_window(self.cgs_window.id(), self.screen.size, &self.root_layer);
        if let Err(error) = self.cgs_window.set_alpha(1.0) {
            warn!(?error, "drop overlay could not show");
        }
        if let Err(error) = self.cgs_window.order_above(None) {
            warn!(?error, "drop overlay could not order above");
        }
    }
}

/// Moves `from` a fraction of the way toward `to`, eased so the region slows as
/// it arrives rather than stopping dead.
fn approach(from: CGRect, to: CGRect, rate: f64) -> CGRect {
    let s = ease(rate.clamp(0.0, 1.0));
    let blend = |a: f64, b: f64| a + (b - a) * s;
    CGRect::new(
        CGPoint::new(
            blend(from.origin.x, to.origin.x),
            blend(from.origin.y, to.origin.y),
        ),
        CGSize::new(
            blend(from.size.width, to.size.width),
            blend(from.size.height, to.size.height),
        ),
    )
}

fn rects_close(a: CGRect, b: CGRect) -> bool {
    (a.origin.x - b.origin.x).abs() < SETTLE_EPSILON
        && (a.origin.y - b.origin.y).abs() < SETTLE_EPSILON
        && (a.size.width - b.size.width).abs() < SETTLE_EPSILON
        && (a.size.height - b.size.height).abs() < SETTLE_EPSILON
}

/// Ease-out: most of the distance early, settling gently.
fn ease(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    #[test]
    fn approach_moves_toward_the_target_and_arrives() {
        let from = rect(0.0, 0.0, 100.0, 100.0);
        let to = rect(200.0, 100.0, 400.0, 300.0);

        let mut current = from;
        for _ in 0..200 {
            current = approach(current, to, 0.35);
        }
        assert!(rects_close(current, to), "never arrived: {current:?}");

        // A single step covers ground without overshooting past the target.
        let one = approach(from, to, 0.35);
        assert!(one.origin.x > from.origin.x && one.origin.x < to.origin.x);
        assert!(one.size.width > from.size.width && one.size.width < to.size.width);
    }

    #[test]
    fn a_full_rate_lands_immediately() {
        let from = rect(0.0, 0.0, 10.0, 10.0);
        let to = rect(50.0, 50.0, 20.0, 20.0);
        assert!(rects_close(approach(from, to, 1.0), to));
    }
}
