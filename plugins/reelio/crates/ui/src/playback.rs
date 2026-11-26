//! Preview transport math and the responsive seek scheduler. Ports of `timeline/playback.ts`
//! and `timeline/seekScheduler.ts`. The scheduler takes the clock as an argument so it is a
//! pure state machine: the host calls `request`/`tick` with its own monotonic milliseconds.

/// Discrete preview speeds offered by the transport slider.
pub const PLAYBACK_SPEEDS: [f64; 10] = [0.1, 0.25, 0.5, 0.75, 1.0, 2.0, 3.0, 4.0, 5.0, 10.0];

/// Frame duration in ms from a rational rate such as `"30000/1001"`; 40 ms (25 fps) fallback.
pub fn preview_frame_ms(rate: Option<&str>) -> f64 {
    let fps = match rate {
        None => f64::NAN,
        Some(r) => {
            let parts: Vec<f64> = r.split('/').filter_map(|p| p.trim().parse().ok()).collect();
            match parts.as_slice() {
                [n, d] if *n > 0.0 && *d > 0.0 => n / d,
                [v] => *v,
                _ => f64::NAN,
            }
        }
    };
    if fps.is_finite() && fps > 0.0 && fps <= 1000.0 {
        1000.0 / fps
    } else {
        40.0
    }
}

pub fn frame_target(current: f64, direction: f64, frame_ms: f64, end: f64) -> f64 {
    (current + direction * frame_ms).clamp(0.0, end.max(0.0))
}

/// One write to the decoder: the time and whether it is a fast (coalesced) intermediate seek.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeekWrite {
    pub time: f64,
    pub fast: bool,
}

/// Coalesces scrub positions to at most one intermediate write per 80 ms, and always releases
/// the exact final target immediately. Stale completions never overwrite a newer target.
#[derive(Debug, Clone)]
pub struct SeekScheduler {
    latest: Option<f64>,
    timer_due: Option<f64>,
    last_time: f64,
    last_target: Option<f64>,
    last_fast: bool,
}

pub const SEEK_INTERVAL_MS: f64 = 80.0;

impl Default for SeekScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl SeekScheduler {
    pub fn new() -> Self {
        Self {
            latest: None,
            timer_due: None,
            last_time: f64::NEG_INFINITY,
            last_target: None,
            last_fast: false,
        }
    }

    pub fn latest(&self) -> Option<f64> {
        self.latest
    }

    pub fn request(&mut self, time: f64, final_: bool, now: f64) -> Vec<SeekWrite> {
        self.latest = Some(time);
        if final_ {
            self.timer_due = None;
            return self.dispatch(false, now).into_iter().collect();
        }
        let wait = SEEK_INTERVAL_MS - (now - self.last_time);
        if wait <= 0.0 {
            self.timer_due = None;
            return self.dispatch(true, now).into_iter().collect();
        }
        if self.timer_due.is_none() {
            self.timer_due = Some(now + wait);
        }
        vec![]
    }

    /// Advance the clock; fires the pending intermediate write when its timer is due.
    pub fn tick(&mut self, now: f64) -> Vec<SeekWrite> {
        match self.timer_due {
            Some(due) if now >= due => {
                self.timer_due = None;
                self.dispatch(true, now).into_iter().collect()
            }
            _ => vec![],
        }
    }

    fn dispatch(&mut self, fast: bool, now: f64) -> Option<SeekWrite> {
        let latest = self.latest?;
        if Some(latest) == self.last_target && (!self.last_fast || fast) {
            return None;
        }
        self.last_time = now;
        self.last_target = Some(latest);
        self.last_fast = fast;
        Some(SeekWrite { time: latest, fast })
    }

    /// Called with the decoder's reported position. Returns `true` once the target is reached.
    pub fn settle(&mut self, actual: f64, seeking: bool, playing: bool, now: f64) -> bool {
        let Some(latest) = self.latest else {
            return true;
        };
        let tolerance = if playing {
            75f64.max(now - self.last_time + 75.0)
        } else {
            75.0
        };
        if !seeking && (actual - latest).abs() <= tolerance {
            self.latest = None;
            self.last_target = None;
            return true;
        }
        false
    }

    pub fn flush(&mut self, now: f64) -> Option<SeekWrite> {
        self.timer_due = None;
        self.dispatch(false, now)
    }

    pub fn reset(&mut self) {
        self.timer_due = None;
        self.latest = None;
        self.last_target = None;
        self.last_time = f64::NEG_INFINITY;
        self.last_fast = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_durations_use_nominal_rates_with_fallback() {
        assert!((preview_frame_ms(Some("30000/1001")) - 33.3666).abs() < 1e-3);
        assert_eq!(preview_frame_ms(Some("25/1")), 40.0);
        assert_eq!(preview_frame_ms(Some("0/0")), 40.0);
        assert_eq!(preview_frame_ms(None), 40.0);
        assert_eq!(preview_frame_ms(Some("24")), 1000.0 / 24.0);
    }

    #[test]
    fn frame_steps_clamp_at_source_boundaries() {
        assert_eq!(frame_target(0.0, -1.0, 40.0, 1000.0), 0.0);
        assert_eq!(frame_target(990.0, 1.0, 40.0, 1000.0), 1000.0);
    }

    #[test]
    fn speeds_cover_the_requested_ladder() {
        assert_eq!(PLAYBACK_SPEEDS.len(), 10);
        assert_eq!(PLAYBACK_SPEEDS[0], 0.1);
        assert_eq!(PLAYBACK_SPEEDS[9], 10.0);
    }

    #[test]
    fn intermediate_seeks_coalesce_and_final_release_is_immediate() {
        let mut s = SeekScheduler::new();
        // The first move has no previous write, so it goes out at once (reference: lastTime = -Infinity).
        let first = s.request(100.0, false, 1000.0);
        assert_eq!(
            first,
            vec![SeekWrite {
                time: 100.0,
                fast: true
            }]
        );
        s.request(200.0, false, 1010.0);
        s.request(300.0, false, 1020.0);
        assert!(
            s.tick(1040.0).is_empty(),
            "no write before the 80 ms interval elapses"
        );
        assert_eq!(
            s.tick(1080.0),
            vec![SeekWrite {
                time: 300.0,
                fast: true
            }],
            "only the latest target is written"
        );
        let release = s.request(400.0, true, 1021.0);
        assert_eq!(
            release,
            vec![SeekWrite {
                time: 400.0,
                fast: false
            }]
        );
        assert!(s.tick(2000.0).is_empty(), "release cancels any queued move");
    }

    #[test]
    fn duplicate_targets_are_not_rewritten_and_stale_completions_are_ignored() {
        let mut s = SeekScheduler::new();
        s.request(500.0, true, 0.0);
        assert!(s.request(500.0, true, 1.0).is_empty(), "deduplicated");
        assert!(
            !s.settle(0.0, false, false, 2.0),
            "stale decode does not settle"
        );
        assert!(s.settle(500.0, false, false, 3.0));
        assert_eq!(s.latest(), None);
    }

    #[test]
    fn a_return_to_an_earlier_seek_is_a_new_target() {
        let mut s = SeekScheduler::new();
        s.request(10.0, true, 0.0);
        s.settle(10.0, false, false, 1.0);
        let back = s.request(10.0, true, 500.0);
        assert_eq!(
            back,
            vec![SeekWrite {
                time: 10.0,
                fast: false
            }]
        );
    }
}
