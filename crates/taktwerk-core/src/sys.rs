//! The Linux calls the cycle thread needs: the monotonic clock, an absolute sleep, and the
//! optional real-time class, affinity and memory lock. The only module with `unsafe`.

// The calls below take plain values or pointers to stack locals this module initialises and
// owns; each block states why it is sound.
#![allow(
    unsafe_code,
    reason = "the engine's single FFI surface: libc clock and scheduling calls"
)]

use std::ffi::c_int;

use crate::clock::{CycleClock, Mono, STOP_POLL};
use crate::project::SchedPolicy;

/// `CLOCK_MONOTONIC`, read with `clock_gettime` and waited on with `clock_nanosleep`.
///
/// `TIMER_ABSTIME` is the point: a relative sleep re-bases on every wake-up and keeps every
/// scheduling delay forever; an absolute deadline discards it.
#[derive(Debug, Clone, Copy, Default)]
pub struct MonotonicClock;

impl MonotonicClock {
    /// `clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, …)`; returns the error number or `0`.
    ///
    /// `EINTR` is reported like any other error: the caller loops on the same absolute
    /// deadline, so an interrupted wait resumes on the same nanosecond.
    fn nanosleep_until(deadline: Mono) -> c_int {
        let spec = libc::timespec {
            // `try_from` rather than `as`, so a narrower `time_t` refuses the wait instead of
            // wrapping into a deadline in the past.
            tv_sec: match libc::time_t::try_from(deadline.as_nanos() / 1_000_000_000) {
                Ok(secs) => secs,
                Err(_) => return libc::EINVAL,
            },
            tv_nsec: (deadline.as_nanos() % 1_000_000_000) as libc::c_long,
        };
        // SAFETY: `spec` is a fully initialised local the call only reads; the remainder
        // pointer may be null, and the absolute form has no remainder to report. The call
        // returns the error number instead of setting `errno`.
        unsafe {
            libc::clock_nanosleep(
                libc::CLOCK_MONOTONIC,
                libc::TIMER_ABSTIME,
                &spec,
                std::ptr::null_mut(),
            )
        }
    }
}

impl CycleClock for MonotonicClock {
    fn now(&self) -> Mono {
        let mut spec = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `spec` is a fully initialised local that only the kernel writes here.
        // `CLOCK_MONOTONIC` always exists on Linux; a failure leaves the zeroes, which the
        // saturating arithmetic of `Mono` turns into a zero interval rather than a wrap.
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut spec);
        }
        let secs = u64::try_from(spec.tv_sec).unwrap_or(0);
        let nanos = u64::try_from(spec.tv_nsec).unwrap_or(0);
        Mono::from_nanos(secs.saturating_mul(1_000_000_000).saturating_add(nanos))
    }

    fn sleep_until(&self, deadline: Mono, stop: &dyn Fn() -> bool) -> bool {
        loop {
            if stop() {
                return true;
            }
            let now = self.now();
            if now >= deadline {
                return false;
            }
            let slice = deadline.min(now.saturating_add(STOP_POLL));
            Self::nanosleep_until(slice);
        }
    }
}

/// The privileged calls of `[engine.realtime]`, behind a seam so a refusal is testable.
pub trait RealtimeSyscalls {
    /// Put the calling thread on `policy` at `priority`. `Err` is the error number.
    fn set_scheduler(&self, policy: SchedPolicy, priority: u8) -> Result<(), c_int>;
    /// Pin the calling thread to `cpu`. `Err` is the error number.
    fn set_affinity(&self, cpu: usize) -> Result<(), c_int>;
    /// `mlockall(MCL_CURRENT | MCL_FUTURE)` for the process. `Err` is the error number.
    fn lock_memory(&self) -> Result<(), c_int>;
}

/// The real calls.
#[derive(Debug, Clone, Copy, Default)]
pub struct LinuxRealtime;

impl RealtimeSyscalls for LinuxRealtime {
    fn set_scheduler(&self, policy: SchedPolicy, priority: u8) -> Result<(), c_int> {
        let policy = match policy {
            SchedPolicy::Fifo => libc::SCHED_FIFO,
            SchedPolicy::Rr => libc::SCHED_RR,
        };
        let param = libc::sched_param {
            sched_priority: c_int::from(priority),
        };
        // SAFETY: `pthread_self` is valid for the calling thread and `param` is a fully
        // initialised local the call only reads. The per-thread call is deliberate: a class
        // set on the process would land on the connector threads too. It returns the error
        // number instead of setting `errno`.
        let rc = unsafe { libc::pthread_setschedparam(libc::pthread_self(), policy, &param) };
        if rc == 0 { Ok(()) } else { Err(rc) }
    }

    fn set_affinity(&self, cpu: usize) -> Result<(), c_int> {
        if cpu >= affinity_width() {
            return Err(libc::EINVAL);
        }
        // SAFETY: `cpu_set_t` is a plain bitmask with no invalid bit pattern; all zeroes is the
        // empty set `CPU_ZERO` would write.
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: `set` is initialised; `CPU_SET` indexes the mask without a bound check, and
        // `cpu` was checked against the mask's width above.
        unsafe { libc::CPU_SET(cpu, &mut set) };
        // SAFETY: thread id `0` names the calling thread; `set` is live and the size passed is
        // its own, so the kernel reads exactly the bytes it was given.
        let rc = unsafe { libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &set) };
        if rc == 0 { Ok(()) } else { Err(errno()) }
    }

    fn lock_memory(&self) -> Result<(), c_int> {
        // SAFETY: `mlockall` takes flags and touches no memory the caller owns.
        let rc = unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) };
        if rc == 0 { Ok(()) } else { Err(errno()) }
    }
}

/// The last error number this thread set.
fn errno() -> c_int {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// The affinity mask's width in CPUs.
#[must_use]
pub fn affinity_width() -> usize {
    size_of::<libc::cpu_set_t>() * 8
}

/// `strerror` text with the number beside it.
#[must_use]
pub fn errno_name(code: c_int) -> String {
    format!("{} (errno {code})", std::io::Error::from_raw_os_error(code))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn monotonic_clock_advances_and_sleeps_until_an_absolute_instant() {
        let clock = MonotonicClock;
        let t0 = clock.now();
        let deadline = t0.saturating_add(Duration::from_millis(5));
        assert!(!clock.sleep_until(deadline, &|| false));
        assert!(
            clock.now() >= deadline,
            "returned before its absolute deadline"
        );
        // A stop request returns at once, however far the deadline is.
        let far = clock.now().saturating_add(Duration::from_secs(60));
        let asked = clock.now();
        assert!(clock.sleep_until(far, &|| true));
        assert!(clock.now().saturating_since(asked) < Duration::from_millis(50));
    }

    #[test]
    fn affinity_past_the_mask_is_refused() {
        assert_eq!(
            LinuxRealtime.set_affinity(affinity_width()),
            Err(libc::EINVAL)
        );
    }
}
