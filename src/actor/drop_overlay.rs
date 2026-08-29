//! Owns the drag drop-region overlay and drives its animation.
//!
//! The overlay is drawn by rendering a layer into a window-server window, which
//! is a snapshot rather than something Core Animation keeps moving, so the
//! motion has to be advanced by hand. This actor holds a frame timer that runs
//! only while the region is still travelling and stops as soon as it settles —
//! a drag that is not moving between drop zones costs nothing.

use std::time::Duration;

use objc2_core_foundation::CGRect;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, instrument, warn};

use crate::actor;
use crate::common::config::Config;
use crate::ui::drop_overlay::{DropOverlayConfig, DropOverlayWindow};

/// Frame interval while the region is moving. Matched to a 60Hz display; the
/// timer is idle whenever nothing is animating.
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Debug)]
pub enum Event {
    /// Point the overlay at a region of `screen`, both in global coordinates.
    Aim {
        screen: CGRect,
        region: CGRect,
    },
    /// The drag ended or left every drop target.
    Hide,
    ConfigUpdated(Config),
}

pub type Sender = actor::Sender<Event>;
pub type Receiver = actor::Receiver<Event>;

pub struct DropOverlay {
    rx: Receiver,
    config: Config,
    window: Option<DropOverlayWindow>,
}

impl DropOverlay {
    pub fn new(config: Config, rx: Receiver) -> Self {
        Self { rx, config, window: None }
    }

    pub async fn run(mut self) {
        let mut frames = interval(FRAME_INTERVAL);
        // Frames are only interesting while something is moving; a late tick
        // should be dropped rather than fired back to back to catch up.
        frames.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut animating = false;

        loop {
            tokio::select! {
                event = self.rx.recv() => {
                    let Some((span, event)) = event else { break };
                    let _guard = span.enter();
                    animating = self.handle_event(event);
                }
                _ = frames.tick(), if animating => {
                    animating = self.window.as_ref().is_some_and(DropOverlayWindow::step);
                }
            }
        }
    }

    fn settings(&self) -> DropOverlayConfig {
        let settings = &self.config.settings.ui.drop_overlay;
        let defaults = DropOverlayConfig::default();
        DropOverlayConfig {
            corner_radius: settings.corner_radius,
            border_width: settings.border_width,
            blur_radius: settings.blur_radius,
            follow_rate: settings.follow_rate,
            ..defaults
        }
    }

    /// Returns whether the overlay still has frames to draw.
    #[instrument(name = "drop_overlay::handle_event", skip(self))]
    fn handle_event(&mut self, event: Event) -> bool {
        match event {
            Event::ConfigUpdated(config) => {
                self.config = config;
                // Colours and blur are baked into the window, so drop it and
                // let the next drag build one with the new settings.
                self.window = None;
                false
            }
            Event::Hide => {
                if let Some(window) = &self.window {
                    window.hide();
                }
                false
            }
            Event::Aim { screen, region } => {
                if !self.config.settings.ui.drop_overlay.enabled {
                    return false;
                }
                // One window per display: a drag that crosses displays needs a
                // window on the one it is over.
                let matches_screen =
                    self.window.as_ref().is_some_and(|window| window.screen() == screen);
                if !matches_screen {
                    if let Some(existing) = self.window.take() {
                        existing.hide();
                    }
                    match DropOverlayWindow::new(screen, self.settings()) {
                        Ok(window) => self.window = Some(window),
                        Err(error) => {
                            warn!(?error, "could not create the drop overlay window");
                            return false;
                        }
                    }
                    debug!(?screen, "drop overlay created");
                }
                let Some(window) = &self.window else {
                    return false;
                };
                window.aim_at(region);
                true
            }
        }
    }
}
