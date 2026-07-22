//! The clock capability: monotonic and wall time as a handed-in trait, so
//! time-dependent behavior is replayable ([`FakeClock`]) and the certified
//! path never consults an ambient clock (see the crate-level doctrine).

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// The clock capability.
pub trait Clock: Send + Sync {
    /// Monotonic time since this clock's own epoch (its construction, for
    /// the std clock). Only differences are meaningful; the epoch is not
    /// comparable across clock instances.
    fn monotonic(&self) -> Duration;

    /// Wall-clock time. Never used on any certified path (wall time is
    /// outside the input closure); provenance timestamps and the Studio use
    /// it.
    fn wall(&self) -> SystemTime;
}

/// The host clock. Monotonic time is measured from construction.
#[derive(Debug)]
pub struct StdClock {
    origin: Instant,
}

impl StdClock {
    /// A clock whose monotonic epoch is now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for StdClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for StdClock {
    fn monotonic(&self) -> Duration {
        self.origin.elapsed()
    }

    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// The test double: time advances only when told to, so timeout logic and
/// journaled timing are exactly reproducible.
#[derive(Debug)]
pub struct FakeClock {
    state: Mutex<(Duration, SystemTime)>,
}

impl FakeClock {
    /// A fake clock at monotonic zero and `SystemTime::UNIX_EPOCH`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new((Duration::ZERO, SystemTime::UNIX_EPOCH)),
        }
    }

    /// Advance both monotonic and wall time by `by`.
    pub fn advance(&self, by: Duration) {
        let mut s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        s.0 += by;
        s.1 += by;
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for FakeClock {
    fn monotonic(&self) -> Duration {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
    }

    fn wall(&self) -> SystemTime {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_clock_advances_only_on_command() {
        let c = FakeClock::new();
        assert_eq!(c.monotonic(), Duration::ZERO);
        assert_eq!(c.wall(), SystemTime::UNIX_EPOCH);
        c.advance(Duration::from_millis(1500));
        assert_eq!(c.monotonic(), Duration::from_millis(1500));
        assert_eq!(
            c.wall(),
            SystemTime::UNIX_EPOCH + Duration::from_millis(1500)
        );
    }

    #[test]
    fn std_clock_is_monotonic() {
        let c = StdClock::new();
        let a = c.monotonic();
        let b = c.monotonic();
        assert!(b >= a);
    }
}
