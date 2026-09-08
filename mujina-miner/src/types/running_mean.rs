//! The mean of the most recent samples of a noisy reading.
//!
//! A fixed-length window over the last `capacity` samples. The mean
//! trails the reading by half the window and shrinks white noise by
//! the square root of the window length. It reports a mean from the
//! first sample on, so a caller sees a value as soon as it has one
//! and a fuller average as the window fills.

use std::collections::VecDeque;

/// A running mean over a fixed number of recent samples.
#[derive(Debug, Clone)]
pub struct RunningMean {
    window: VecDeque<f32>,
    capacity: usize,
}

impl RunningMean {
    /// A window holding the last `capacity` samples. Panics on a
    /// capacity of zero.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "a running mean needs at least one sample");
        Self {
            window: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Adds a sample, dropping the oldest once the window is full.
    pub fn push(&mut self, sample: f32) {
        if self.window.len() == self.capacity {
            self.window.pop_front();
        }
        self.window.push_back(sample);
    }

    /// The mean of the samples in the window, or `None` when empty.
    pub fn mean(&self) -> Option<f32> {
        if self.window.is_empty() {
            return None;
        }
        Some(self.window.iter().sum::<f32>() / self.window.len() as f32)
    }

    /// The number of samples in the window.
    pub fn len(&self) -> usize {
        self.window.len()
    }

    /// Whether the window holds no samples.
    pub fn is_empty(&self) -> bool {
        self.window.is_empty()
    }

    /// Empties the window.
    pub fn clear(&mut self) {
        self.window.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_window_has_no_mean() {
        let m = RunningMean::new(4);
        assert_eq!(m.mean(), None);
        assert!(m.is_empty());
    }

    #[test]
    fn mean_covers_the_partial_window_then_slides() {
        let mut m = RunningMean::new(3);
        m.push(10.0);
        assert_eq!(m.mean(), Some(10.0));
        m.push(20.0);
        assert_eq!(m.mean(), Some(15.0));
        m.push(30.0);
        assert_eq!(m.mean(), Some(20.0));
        // The window is full; the 10 falls out
        m.push(40.0);
        assert_eq!(m.mean(), Some(30.0));
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn one_outlier_moves_the_mean_by_its_share() {
        let mut m = RunningMean::new(8);
        for _ in 0..7 {
            m.push(56.0);
        }
        m.push(76.0);
        assert!((m.mean().unwrap() - 58.5).abs() < 1e-4);
    }

    #[test]
    fn clear_forgets_everything() {
        let mut m = RunningMean::new(2);
        m.push(1.0);
        m.clear();
        assert_eq!(m.mean(), None);
        m.push(5.0);
        assert_eq!(m.mean(), Some(5.0));
    }

    #[test]
    #[should_panic]
    fn zero_capacity_is_refused() {
        RunningMean::new(0);
    }
}
