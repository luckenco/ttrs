//! Suspend-aware server-time bounds. No remote time is derived from the device wall clock.
use crate::Timestamp;
use std::io;

/// A sample from one suspend-aware clock domain, in nanoseconds.
/// Plain values also permit deterministic tests without a second clock architecture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sample {
    pub domain: u64,
    pub nanos: u64,
}

/// A server sample lies somewhere within the bootstrap round trip.
#[derive(Clone, Copy, Debug)]
pub struct Anchor {
    server: Timestamp,
    received: Sample,
    uncertainty_ns: u64,
}
impl Anchor {
    /// Reject bootstrap round trips over five seconds, clock changes, and regressions.
    pub fn new(server: Timestamp, sent: Sample, received: Sample) -> Option<Self> {
        if sent.domain != received.domain {
            return None;
        }
        let uncertainty_ns = received.nanos.checked_sub(sent.nanos)?;
        if uncertainty_ns > 5_000_000_000 {
            return None;
        }
        Some(Self {
            server,
            received,
            uncertainty_ns,
        })
    }
    /// Inclusive server-time bounds; anchors expire thirty seconds after receipt,
    /// including time spent suspended. A discontinuity fails closed.
    pub fn bounds(&self, now: Sample) -> Option<(Timestamp, Timestamp)> {
        if now.domain != self.received.domain {
            return None;
        }
        let age = now.nanos.checked_sub(self.received.nanos)?;
        if age > 30_000_000_000 {
            return None;
        }
        let lower = self.server.as_nanosecond().checked_add(i128::from(age))?;
        let upper = lower.checked_add(i128::from(self.uncertainty_ns))?;
        Some((
            Timestamp::from_nanosecond(lower).ok()?,
            Timestamp::from_nanosecond(upper).ok()?,
        ))
    }
    /// Reference for relative parsing. Refuse an uncertain local calendar date.
    /// Callers should refresh first; refreshing never substitutes device time.
    pub fn reference(&self, now: Sample, zone: &jiff::tz::TimeZone) -> Option<Timestamp> {
        let (lower, upper) = self.bounds(now)?;
        if lower.to_zoned(zone.clone()).date() != upper.to_zoned(zone.clone()).date() {
            return None;
        }
        Timestamp::from_microsecond(lower.as_microsecond()).ok()
    }
}

/// Sample the OS clock that includes suspend. Unsupported platforms fail closed.
#[cfg(target_os = "linux")]
pub fn sample() -> io::Result<Sample> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: value is a valid writable timespec; CLOCK_BOOTTIME includes suspend.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut value) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let nanos = u64::try_from(value.tv_sec)
        .ok()
        .and_then(|s| s.checked_mul(1_000_000_000))
        .and_then(|s| {
            u64::try_from(value.tv_nsec)
                .ok()
                .and_then(|n| s.checked_add(n))
        })
        .ok_or_else(|| io::Error::other("invalid suspend clock"))?;
    Ok(Sample { domain: 1, nanos })
}
#[cfg(target_os = "macos")]
pub fn sample() -> io::Result<Sample> {
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_timebase_info(info: *mut Timebase) -> i32;
        fn mach_continuous_time() -> u64;
    }
    let mut info = Timebase { numer: 0, denom: 0 };
    // SAFETY: OS clock functions accept this valid writable timebase structure.
    let ticks = unsafe {
        if mach_timebase_info(&mut info) != 0 || info.denom == 0 {
            return Err(io::Error::other("invalid suspend clock timebase"));
        }
        mach_continuous_time()
    };
    let nanos = u128::from(ticks)
        .checked_mul(u128::from(info.numer))
        .and_then(|n| n.checked_div(u128::from(info.denom)))
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| io::Error::other("suspend clock overflow"))?;
    Ok(Sample {
        domain: (u64::from(info.numer) << 32) | u64::from(info.denom),
        nanos,
    })
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn sample() -> io::Result<Sample> {
    Err(io::Error::other("suspend-aware clock unavailable"))
}
