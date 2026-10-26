//! Progress, cancellation, speed and ETA for a project upload. Shared by the GUI and the CLI.

use std::{
    sync::{
        atomic::{AtomicU64, AtomicU8, Ordering::Relaxed},
        Arc,
    },
    time::{Duration, Instant},
};

use tokio_util::sync::CancellationToken;

/// Returned (inside `anyhow::Error`) when the user cancelled; check with `err.is::<Cancelled>()`.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// Measuring the project size.
    Scanning,
    /// Building the archive on this computer.
    Packing,
    /// Sending it to the VPS.
    Uploading,
    /// All bytes sent; the VPS is unpacking.
    Finishing,
    Done,
    /// Reading local files to tell what changed.
    Hashing,
    Downloading,
    /// Checking a finished download against its checksum.
    Verifying,
}

impl Phase {
    fn to_u8(self) -> u8 {
        self as u8
    }

    fn from_u8(v: u8) -> Phase {
        match v {
            1 => Phase::Scanning,
            2 => Phase::Packing,
            3 => Phase::Uploading,
            4 => Phase::Finishing,
            5 => Phase::Done,
            6 => Phase::Hashing,
            7 => Phase::Downloading,
            8 => Phase::Verifying,
            _ => Phase::Idle,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    pub phase: Phase,
    pub total: u64,
    pub done: u64,
}

impl Snapshot {
    /// 0.0 - 1.0
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f64 / self.total as f64).min(1.0) as f32
        }
    }
}

/// Shared between the task doing the work and whoever displays it. All reads are lock-free.
pub struct Transfer {
    phase: AtomicU8,
    total: AtomicU64,
    done: AtomicU64,
    token: CancellationToken,
}

impl Transfer {
    pub fn new() -> Arc<Transfer> {
        Arc::new(Transfer { phase: AtomicU8::new(0), total: AtomicU64::new(0), done: AtomicU64::new(0), token: CancellationToken::new() })
    }

    pub fn snapshot(&self) -> Snapshot {
        let phase = Phase::from_u8(self.phase.load(Relaxed));
        let (total, done) = (self.total.load(Relaxed), self.done.load(Relaxed));
        // Everything is on the wire but the VPS has not answered yet: it is unpacking.
        let phase = if phase == Phase::Uploading && total > 0 && done >= total { Phase::Finishing } else { phase };
        Snapshot { phase, total, done }
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.token.cancelled().await
    }

    pub(crate) fn set_phase(&self, p: Phase) {
        self.phase.store(p.to_u8(), Relaxed);
    }

    pub(crate) fn begin(&self, p: Phase, total: u64) {
        self.total.store(total, Relaxed);
        self.done.store(0, Relaxed);
        self.set_phase(p);
    }

    /// Like `begin`, but for a transfer that continues from `done` bytes (a resumed download).
    pub(crate) fn begin_at(&self, p: Phase, total: u64, done: u64) {
        self.total.store(total, Relaxed);
        self.done.store(done, Relaxed);
        self.set_phase(p);
    }

    pub(crate) fn add_done(&self, n: u64) {
        self.done.fetch_add(n, Relaxed);
    }
}

/// Smoothed transfer speed from periodic samples of the byte counter.
pub struct SpeedMeter {
    last: Instant,
    last_done: u64,
    bps: f64,
    /// The first sample only sets the baseline, so a resumed transfer (which starts at N bytes) does not look fast.
    primed: bool,
}

impl SpeedMeter {
    pub fn new() -> Self {
        SpeedMeter { last: Instant::now(), last_done: 0, bps: 0.0, primed: false }
    }

    /// Call every few hundred ms with the current byte count; returns bytes/second.
    pub fn sample(&mut self, done: u64) -> f64 {
        let now = Instant::now();
        if !self.primed {
            self.primed = true;
            self.last = now;
            self.last_done = done;
            return self.bps;
        }
        let dt = now.duration_since(self.last).as_secs_f64();
        if dt < 0.2 {
            return self.bps;
        }
        let instant = done.saturating_sub(self.last_done) as f64 / dt;
        // Exponential moving average: steady enough to read, quick enough to follow changes.
        self.bps = if self.bps == 0.0 { instant } else { self.bps * 0.7 + instant * 0.3 };
        self.last = now;
        self.last_done = done;
        self.bps
    }

    /// A new phase restarts the counter.
    pub fn reset(&mut self) {
        *self = SpeedMeter::new();
    }
}

impl Default for SpeedMeter {
    fn default() -> Self {
        Self::new()
    }
}

pub fn eta(total: u64, done: u64, bps: f64) -> Option<Duration> {
    if bps < 1.0 || done >= total {
        return None;
    }
    Some(Duration::from_secs_f64((total - done) as f64 / bps))
}

pub fn fmt_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

pub fn fmt_rate(bps: f64) -> String {
    format!("{}/s", fmt_bytes(bps as u64))
}

pub fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1536), "1.5 KB");
        assert_eq!(fmt_bytes(5 * 1024 * 1024 * 1024), "5.0 GB");
        assert_eq!(fmt_duration(Duration::from_secs(5)), "5s");
        assert_eq!(fmt_duration(Duration::from_secs(65)), "1m 05s");
        assert_eq!(fmt_duration(Duration::from_secs(3720)), "1h 02m");
    }

    #[test]
    fn eta_needs_a_speed() {
        assert_eq!(eta(100, 0, 0.0), None);
        assert_eq!(eta(100, 100, 10.0), None);
        assert_eq!(eta(100, 50, 10.0), Some(Duration::from_secs(5)));
    }

    #[test]
    fn snapshot_reports_finishing_when_all_bytes_are_sent() {
        let t = Transfer::new();
        t.begin(Phase::Uploading, 100);
        t.add_done(40);
        assert_eq!(t.snapshot().phase, Phase::Uploading);
        t.add_done(60);
        assert_eq!(t.snapshot().phase, Phase::Finishing);
        assert!((t.snapshot().fraction() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn cancel_is_observable() {
        let t = Transfer::new();
        assert!(!t.is_cancelled());
        t.cancel();
        assert!(t.is_cancelled());
    }
}

#[cfg(test)]
mod meter_tests {
    use super::*;

    #[test]
    fn first_sample_is_only_a_baseline() {
        let mut m = SpeedMeter::new();
        // A resumed download starts at 5 GB; that must not be reported as a huge speed.
        assert_eq!(m.sample(5_000_000_000), 0.0);
        std::thread::sleep(std::time::Duration::from_millis(250));
        let bps = m.sample(5_000_000_000 + 2_500_000);
        assert!(bps > 5_000_000.0 && bps < 20_000_000.0, "got {bps}");
    }
}
