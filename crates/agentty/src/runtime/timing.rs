//! Runtime timing constants shared by the render loop and event reader.

use std::time::Duration;

/// Target redraw and input-poll cadence for the terminal runtime at 30 FPS.
pub(crate) const FRAME_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 30);
