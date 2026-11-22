//! The numbers behind the recording bar: how loud a bar is, how long a recording is, and the
//! timer text. Pure arithmetic, so every edge can be tested.
use std::time::Duration;

/// Quieter than this reads as silence.
const FLOOR_DB: f64 = -60.0;

/// The height of one waveform bar, `0.0..=1.0`, for a peak level in decibels (0 dB is the loudest
/// a signal can be, `-inf` is silence). Speech peaks at around -30 to -6 dB; the square root of
/// the amplitude lifts the quiet end so ordinary talking moves the bars visibly.
pub fn bar_height(peak_db: f64) -> f32 {
    if peak_db.is_nan() || peak_db <= FLOOR_DB {
        return 0.0;
    }
    let amplitude = 10f64.powf(peak_db.min(0.0) / 20.0);
    amplitude.sqrt().clamp(0.0, 1.0) as f32
}

/// `00:00`: minutes and seconds, each two digits, from a recording's length. The seconds are
/// rounded down so the timer never runs ahead of what has been recorded.
pub fn timer_text(length: Duration) -> String {
    let seconds = length.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

/// The most recent bars, newest last, in a fixed amount of memory.
#[derive(Clone, Debug)]
pub struct History {
    bars: Vec<f32>,
    next: usize,
    filled: usize,
}

impl History {
    pub fn new(capacity: usize) -> Self {
        Self { bars: vec![0.0; capacity.max(1)], next: 0, filled: 0 }
    }
    pub fn capacity(&self) -> usize {
        self.bars.len()
    }
    pub fn len(&self) -> usize {
        self.filled
    }
    pub fn push(&mut self, bar: f32) {
        self.bars[self.next] = if bar.is_finite() { bar.clamp(0.0, 1.0) } else { 0.0 };
        self.next = (self.next + 1) % self.bars.len();
        self.filled = (self.filled + 1).min(self.bars.len());
    }
    pub fn clear(&mut self) {
        self.next = 0;
        self.filled = 0;
    }
    /// The newest `count` bars (fewer if fewer exist), oldest first.
    pub fn recent(&self, count: usize) -> impl Iterator<Item = f32> + '_ {
        let count = count.min(self.filled);
        let capacity = self.bars.len();
        (0..count).map(move |i| self.bars[(self.next + capacity - count + i) % capacity])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bar_follows_the_level_from_silence_to_full_scale() {
        assert_eq!(bar_height(f64::NEG_INFINITY), 0.0);
        assert_eq!(bar_height(-60.0), 0.0);
        assert_eq!(bar_height(-61.0), 0.0);
        assert_eq!(bar_height(0.0), 1.0);
        let steps: Vec<f32> = (-59..=0).map(|db| bar_height(f64::from(db))).collect();
        assert!(steps.windows(2).all(|pair| pair[0] <= pair[1]), "louder is never shorter");
        assert!(steps.iter().all(|bar| (0.0..=1.0).contains(bar)));
        // Ordinary speech is clearly visible without filling the bar.
        assert!(bar_height(-20.0) > 0.25 && bar_height(-20.0) < 0.5, "{}", bar_height(-20.0));
        assert!(bar_height(-6.0) > 0.6 && bar_height(-6.0) < 0.85);
        assert!(bar_height(-50.0) > 0.0 && bar_height(-50.0) < 0.1, "a whisper moves it a little");
    }

    #[test]
    fn odd_levels_never_produce_a_bad_bar() {
        for odd in [f64::NAN, f64::INFINITY, 12.0, 1e300, -1e300, f64::MIN, f64::MAX] {
            let bar = bar_height(odd);
            assert!(bar.is_finite() && (0.0..=1.0).contains(&bar), "{odd}: {bar}");
        }
        assert_eq!(bar_height(f64::NAN), 0.0);
        assert_eq!(bar_height(6.0), 1.0, "over full scale is clipped, not larger");
    }

    #[test]
    fn the_timer_reads_minutes_and_seconds_with_two_digits_each() {
        for (millis, want) in [
            (0, "00:00"),
            (999, "00:00"),
            (1000, "00:01"),
            (9_999, "00:09"),
            (59_999, "00:59"),
            (60_000, "01:00"),
            (125_000, "02:05"),
            (599_999, "09:59"),
            (600_000, "10:00"),
            (3_599_000, "59:59"),
            (3_600_000, "60:00"),
            (6_000_000, "100:00"),
        ] {
            assert_eq!(timer_text(Duration::from_millis(millis)), want, "{millis} ms");
        }
        // It never runs ahead of the recording.
        assert_eq!(timer_text(Duration::from_micros(1_999_999)), "00:01");
        assert!(timer_text(Duration::MAX).len() > 4, "no overflow on absurd lengths");
    }

    #[test]
    fn the_history_keeps_only_the_newest_bars_in_order() {
        let mut history = History::new(4);
        assert_eq!((history.len(), history.capacity()), (0, 4));
        assert_eq!(history.recent(10).count(), 0);
        for bar in [0.1, 0.2] {
            history.push(bar);
        }
        assert_eq!(history.recent(10).collect::<Vec<_>>(), [0.1, 0.2]);
        for bar in [0.3, 0.4, 0.5, 0.6] {
            history.push(bar);
        }
        assert_eq!(history.len(), 4, "memory is fixed");
        assert_eq!(history.recent(10).collect::<Vec<_>>(), [0.3, 0.4, 0.5, 0.6], "the oldest were dropped");
        assert_eq!(history.recent(2).collect::<Vec<_>>(), [0.5, 0.6], "asking for fewer gives the newest");
        assert_eq!(history.recent(0).count(), 0);
        history.clear();
        assert_eq!(history.len(), 0);
        history.push(0.9);
        assert_eq!(history.recent(4).collect::<Vec<_>>(), [0.9]);
    }

    #[test]
    fn the_history_cleans_what_it_is_given_and_survives_a_huge_run() {
        let mut history = History::new(3);
        for bad in [f32::NAN, f32::INFINITY, -4.0, 7.0] {
            history.push(bad);
        }
        // NaN, infinity and a negative level all became silence; a level over full scale, full.
        assert_eq!(history.recent(3).collect::<Vec<_>>(), [0.0, 0.0, 1.0]);
        let mut big = History::new(8);
        for n in 0..1_000_000u32 {
            big.push((n % 100) as f32 / 100.0);
        }
        assert_eq!(big.len(), 8);
        assert_eq!(big.recent(8).count(), 8);
        assert_eq!(History::new(0).capacity(), 1, "never empty, so pushing cannot divide by zero");
    }
}
