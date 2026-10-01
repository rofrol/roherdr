//! Animated status glyphs: a working agent and a running job turn as half
//! circles, in opposite directions and at different speeds, so the two stay
//! apart without the hourglass the job used to have. Presentation only: the
//! client advances the phase from its own clock, nothing here reaches the
//! server, and everything stays one cell wide in every frame.
//!
//! The phase is data the client sets for the duration of a compose (see
//! [`scope`]); the glyph functions read it, so the many places that draw a
//! status icon need no extra argument. Outside a scope, and with motion
//! disabled, the glyphs are static: `◐` for working and `◑` for a job.

use std::cell::Cell;
use std::time::Duration;

/// Working agent: clockwise.
const WORKING_FRAMES: [&str; 4] = ["◐", "◓", "◑", "◒"];
/// Running job: counter-clockwise.
const JOB_FRAMES: [&str; 4] = ["◐", "◒", "◑", "◓"];
const WORKING_PERIOD: Duration = Duration::from_millis(160);
const JOB_PERIOD: Duration = Duration::from_millis(320);
const WORKING_STATIC: &str = WORKING_FRAMES[0];
const JOB_STATIC: &str = "◑";

/// Which frame each kind of glyph shows, or static glyphs when not `enabled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Motion {
    enabled: bool,
    working: usize,
    job: usize,
}

impl Motion {
    pub(crate) const STATIC: Self = Self {
        enabled: false,
        working: 0,
        job: 0,
    };

    /// The frames `elapsed` after the epoch both loops started together.
    pub(crate) fn at(elapsed: Duration) -> Self {
        let frame = |period: Duration| (elapsed.as_millis() / period.as_millis()) as usize % 4;
        Self {
            enabled: true,
            working: frame(WORKING_PERIOD),
            job: frame(JOB_PERIOD),
        }
    }

    /// Time from `elapsed` to the next frame change of either loop.
    pub(crate) fn until_next_frame(elapsed: Duration) -> Duration {
        let until = |period: Duration| {
            let period = period.as_millis();
            Duration::from_millis((period - elapsed.as_millis() % period) as u64)
        };
        until(WORKING_PERIOD).min(until(JOB_PERIOD))
    }
}

thread_local! {
    static CURRENT: Cell<Motion> = const { Cell::new(Motion::STATIC) };
}

/// Restores the previous phase when dropped.
pub(crate) struct Scope(Motion);

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|current| current.set(self.0));
    }
}

/// Makes `motion` the phase glyphs are drawn with, until the guard drops.
pub(crate) fn scope(motion: Motion) -> Scope {
    Scope(CURRENT.with(|current| current.replace(motion)))
}

/// The half circle of a working agent.
pub(crate) fn working_glyph() -> &'static str {
    let motion = CURRENT.with(Cell::get);
    if motion.enabled {
        WORKING_FRAMES[motion.working % 4]
    } else {
        WORKING_STATIC
    }
}

/// The half circle of a running job, where the hourglass used to be.
pub(crate) fn job_glyph() -> &'static str {
    let motion = CURRENT.with(Cell::get);
    if motion.enabled {
        JOB_FRAMES[motion.job % 4]
    } else {
        JOB_STATIC
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn working_turns_clockwise_and_a_job_counter_clockwise_and_slower() {
        let shown = |at: u64| {
            let _scope = scope(Motion::at(ms(at)));
            (working_glyph(), job_glyph())
        };
        assert_eq!(shown(0), ("◐", "◐"));
        assert_eq!(shown(160), ("◓", "◐"));
        assert_eq!(shown(320), ("◑", "◒"));
        assert_eq!(shown(480), ("◒", "◒"));
        assert_eq!(shown(640), ("◐", "◑"));
        // Both loops repeat after 1.28 s.
        assert_eq!(shown(1_280), ("◐", "◐"));
    }

    #[test]
    fn without_a_scope_or_with_motion_off_the_glyphs_are_static_and_distinct() {
        assert_eq!((working_glyph(), job_glyph()), ("◐", "◑"));
        let _scope = scope(Motion::STATIC);
        assert_eq!((working_glyph(), job_glyph()), ("◐", "◑"));
    }

    #[test]
    fn a_scope_restores_the_previous_phase() {
        let outer = scope(Motion::at(ms(160)));
        {
            let _inner = scope(Motion::at(ms(0)));
            assert_eq!(working_glyph(), "◐");
        }
        assert_eq!(working_glyph(), "◓");
        drop(outer);
        assert_eq!(working_glyph(), "◐");
    }

    #[test]
    fn the_next_frame_comes_at_the_nearest_boundary() {
        assert_eq!(Motion::until_next_frame(ms(0)), ms(160));
        assert_eq!(Motion::until_next_frame(ms(150)), ms(10));
        assert_eq!(Motion::until_next_frame(ms(170)), ms(150));
        assert_eq!(Motion::until_next_frame(ms(319)), ms(1));
    }
}
