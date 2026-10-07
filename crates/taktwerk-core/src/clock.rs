//! The cycle's notion of time: a monotonic reading, a clock seam, and the absolute deadline grid.
//!
//! The scheduler reads time only through [`CycleClock`], so a test drives it with a counter and
//! asserts the grid exactly; production uses [`crate::sys::MonotonicClock`].

use std::time::Duration;

/// The longest one wait lasts before the stop flag is looked at again.
///
/// Every wait names an absolute instant, so slicing it changes nothing about the grid; it only
/// bounds how long a stop request waits at a long period.
pub const STOP_POLL: Duration = Duration::from_millis(250);

/// A reading of the monotonic clock, nanoseconds since its epoch.
///
/// A plain number rather than [`std::time::Instant`], so a fake clock can produce any value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Mono(u64);

impl Mono {
    /// A reading at `nanos`.
    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Nanoseconds since the clock's epoch.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// How long after `earlier` this reading is; zero when it is not after it.
    #[must_use]
    pub fn saturating_since(self, earlier: Self) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }

    /// This reading advanced by `d`, clamped at the end of the range.
    #[must_use]
    pub fn saturating_add(self, d: Duration) -> Self {
        Self(self.0.saturating_add(nanos_of(d)))
    }
}

/// A `Duration` as nanoseconds, clamped.
#[must_use]
pub fn nanos_of(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// The clock the cycle thread reads and sleeps on. Nothing in here may allocate.
pub trait CycleClock {
    /// Read the monotonic clock.
    fn now(&self) -> Mono;

    /// Wait until `deadline` or until `stop` returns `true`, whichever comes first.
    ///
    /// Returns `true` when it returned because of `stop`. `stop` is polled at least every
    /// [`STOP_POLL`]; a deadline already in the past returns at once.
    fn sleep_until(&self, deadline: Mono, stop: &dyn Fn() -> bool) -> bool;
}

/// The absolute grid the cycle runs on: slot `k` starts at `start + k · period`.
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    period: Duration,
    /// The instant the current cycle was due.
    slot: Mono,
}

impl Deadline {
    /// A grid whose first slot is `start`.
    #[must_use]
    pub fn start(start: Mono, period: Duration) -> Self {
        Self {
            period,
            slot: start,
        }
    }

    /// The instant the next cycle is due.
    #[must_use]
    pub fn next(&self) -> Mono {
        self.slot
    }

    /// The period.
    #[must_use]
    pub fn period(&self) -> Duration {
        self.period
    }

    /// Step to the next slot, skipping every slot `now` has already passed.
    ///
    /// Returns how many slots were missed: zero when `now` is at or before the next slot, so a
    /// cycle that used its whole budget was still on time. Closed form, so a long stall costs
    /// one division.
    pub fn advance(&mut self, now: Mono) -> u32 {
        self.slot = self.slot.saturating_add(self.period);
        if self.slot >= now {
            return 0;
        }
        let period_ns = nanos_of(self.period).max(1);
        let behind = now.as_nanos().saturating_sub(self.slot.as_nanos());
        // `div_ceil` lands on the first slot at or after `now`.
        let slots = behind.div_ceil(period_ns);
        self.slot = Mono::from_nanos(
            self.slot
                .as_nanos()
                .saturating_add(slots.saturating_mul(period_ns)),
        );
        u32::try_from(slots).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_steps_one_slot_and_skips_only_what_is_past() {
        let period = Duration::from_millis(100);
        let base = Mono::from_nanos(1_000_000_000);
        let p = nanos_of(period);

        let mut grid = Deadline::start(base, period);
        assert_eq!(
            grid.advance(base.saturating_add(Duration::from_millis(99))),
            0
        );
        assert_eq!(grid.next(), base.saturating_add(period));

        let mut grid = Deadline::start(base, period);
        assert_eq!(grid.advance(base.saturating_add(period)), 0);
        assert_eq!(grid.next(), base.saturating_add(period));

        let mut grid = Deadline::start(base, period);
        assert_eq!(grid.advance(Mono::from_nanos(base.as_nanos() + p + 1)), 1);
        assert_eq!(grid.next().as_nanos(), base.as_nanos() + 2 * p);

        let mut grid = Deadline::start(base, period);
        assert_eq!(
            grid.advance(base.saturating_add(Duration::from_millis(250))),
            2
        );
        assert_eq!(grid.next().as_nanos(), base.as_nanos() + 3 * p);

        let mut grid = Deadline::start(base, period);
        assert_eq!(
            grid.advance(base.saturating_add(Duration::from_secs(3_600))),
            35_999
        );
        assert_eq!(grid.next().as_nanos(), base.as_nanos() + 36_000 * p);
    }

    #[test]
    fn mono_arithmetic_saturates() {
        let a = Mono::from_nanos(10);
        let b = Mono::from_nanos(4);
        assert_eq!(a.saturating_since(b), Duration::from_nanos(6));
        assert_eq!(b.saturating_since(a), Duration::ZERO);
        assert_eq!(
            Mono::from_nanos(u64::MAX)
                .saturating_add(Duration::from_secs(1))
                .as_nanos(),
            u64::MAX
        );
    }
}
