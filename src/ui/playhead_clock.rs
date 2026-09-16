//! Animate the cursor between media-clock observations, without running ahead of a stalled player.
use std::time::Instant;

#[derive(Default)]
pub(super) struct PlayheadClock {
    sample: Option<(f64, Instant)>,
    tick: Option<Instant>,
    shown: f64,
}

impl PlayheadClock {
    pub fn reset(&mut self) {
        self.sample = None;
        self.tick = None;
    }

    pub fn position(&mut self, position: f64, playing: bool, now: Instant) -> f64 {
        if !playing || self.sample.is_none() {
            self.sample = Some((position, now));
            self.tick = Some(now);
            self.shown = position;
            return position;
        }
        let (last, _) = self.sample.unwrap();
        if (position - last).abs() > f64::EPSILON {
            self.sample = Some((position, now));
        }
        let (sample, sampled_at) = self.sample.unwrap();
        let elapsed = now
            .duration_since(self.tick.replace(now).unwrap())
            .as_secs_f64();
        let estimate = sample + now.duration_since(sampled_at).as_secs_f64().min(0.15);
        if elapsed > 0.25 || (estimate - self.shown).abs() > 0.5 {
            self.shown = estimate;
        } else {
            // Correct small media-clock jitter by changing speed slightly, never
            // jumping backwards each time an older timestamp arrives.
            let next = self.shown + elapsed;
            let correction = (estimate - next).clamp(-elapsed * 0.2, elapsed * 0.2);
            self.shown = (next + correction).min(sample + 0.15);
        }
        self.shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn cursor_advances_between_sparse_media_updates() {
        let now = Instant::now();
        let mut clock = PlayheadClock::default();
        clock.position(1.0, true, now);
        let a = clock.position(1.0, true, now + Duration::from_millis(16));
        let b = clock.position(1.0, true, now + Duration::from_millis(32));
        let c = clock.position(1.04, true, now + Duration::from_millis(48));
        assert!(1.0 < a && a < b && b < c);
        assert!((c - 1.048).abs() < 0.01);
    }

    #[test]
    fn stalled_media_does_not_let_cursor_run_away() {
        let now = Instant::now();
        let mut clock = PlayheadClock::default();
        clock.position(1.0, true, now);
        for frame in 1..200 {
            assert!(clock.position(1.0, true, now + Duration::from_millis(frame * 16)) <= 1.15);
        }
    }

    #[test]
    fn pause_and_seek_use_the_authoritative_position() {
        let now = Instant::now();
        let mut clock = PlayheadClock::default();
        clock.position(1.0, true, now);
        assert_eq!(
            clock.position(1.03, false, now + Duration::from_millis(40)),
            1.03
        );
        clock.reset();
        assert_eq!(
            clock.position(0.4, true, now + Duration::from_millis(50)),
            0.4
        );
    }
}
