//! Process-global performance profiler — port of `src/profiler.{h,cpp}`.
//!
//! A lightweight aggregating profiler that records `(name, duration)` samples
//! from any thread and aggregates per-name `total/min/max/last/count` nanos.
//! Read via [`Profiler::snapshot`] for display ([`crate::ui::dialogs`]'s
//! `ProfilerDialog`). [`Profiler::reset`] clears everything. Two operating
//! modes mirror the C++ exactly:
//!
//! * **Disabled (default):** [`Profiler::record`] is an early-return no-op, so
//!   wrapping a hot path with [`PROFILE_SCOPE`] costs a branch + return when
//!   profiling is off in production builds.
//! * **Enabled:** [`Profiler::record`] takes the mutex and updates the bucket.
//!
//! Enable with [`Profiler::set_enabled(true)`](Profiler::set_enabled) from the
//! dialog (which auto-enables on open).
//!
//! Unlike the rest of `ThemeManager` (UI-thread-owned), this is a real
//! **process-global singleton** (`OnceLock<Mutex<…>>` + an `AtomicBool` flag),
//! exactly like the C++ Meyers singleton — `compose` / `controller` / `editor`
//! hot paths on the gpui background executor must reach it lock-free when
//! disabled. It lives in the always-on `theme` module so it builds with
//! `--no-default-features` (no gpui) and is reachable everywhere.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Per-bucket aggregated timing stats — port of `struct ProfileStats`
/// (`profiler.h:12-18`). All durations are nanoseconds.
#[derive(Clone, Copy, Debug)]
pub struct ProfileStats {
    /// Sum of all sample durations (ns).
    pub total_ns: u64,
    /// Smallest sample seen (ns). `u64::MAX` sentinel until the first sample
    /// (matches the C++ `std::numeric_limits<qint64>::max()` init).
    pub min_ns: u64,
    /// Largest sample seen (ns).
    pub max_ns: u64,
    /// Most recent sample (ns) — useful for a live overlay.
    pub last_ns: u64,
    /// Number of samples.
    pub count: u64,
}

impl Default for ProfileStats {
    fn default() -> Self {
        ProfileStats {
            total_ns: 0,
            min_ns: u64::MAX,
            max_ns: 0,
            last_ns: 0,
            count: 0,
        }
    }
}

impl ProfileStats {
    /// Mean sample duration (ns); `0.0` when no samples recorded yet. Mirrors
    /// the C++ `count ? double(totalNs) / count : 0`.
    pub fn mean_ns(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.total_ns as f64 / self.count as f64
        }
    }

    /// `min_ns` reported as `0` before the first sample (the dialog never shows
    /// the `u64::MAX` sentinel; the C++ table only renders rows with `count>0`).
    pub fn min_ns_display(&self) -> u64 {
        if self.count == 0 {
            0
        } else {
            self.min_ns
        }
    }
}

/// The process-global profiler state (the singleton's interior).
struct ProfilerState {
    stats: Mutex<HashMap<&'static str, ProfileStats>>,
    enabled: AtomicBool,
}

fn state() -> &'static ProfilerState {
    static INST: OnceLock<ProfilerState> = OnceLock::new();
    INST.get_or_init(|| ProfilerState {
        stats: Mutex::new(HashMap::new()),
        enabled: AtomicBool::new(false),
    })
}

/// `class Profiler` (`profiler.h:31-47`). A zero-sized handle onto the
/// process-global singleton (`Profiler::instance()` becomes free functions on
/// this handle, kept as an associated-fn API for source parity).
pub struct Profiler;

impl Profiler {
    /// `setEnabled(on)` (`profiler.h:35`). Relaxed store — the flag is a hint,
    /// not a synchronization point.
    pub fn set_enabled(on: bool) {
        state().enabled.store(on, Ordering::Relaxed);
    }

    /// `isEnabled()` (`profiler.h:36`). Relaxed load.
    pub fn is_enabled() -> bool {
        state().enabled.load(Ordering::Relaxed)
    }

    /// `record(name, nanos)` (`profiler.cpp:11-20`). Dropped if disabled
    /// between scope start and end. `name` MUST be a `'static` string (the C++
    /// captures the literal by pointer); we key the map by `&'static str` so
    /// there is no per-sample allocation when enabled.
    pub fn record(name: &'static str, nanos: u64) {
        if !Self::is_enabled() {
            return; // dropped if disabled between scope start and end
        }
        let mut map = match state().stats.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(), // never poison the profiler over a panic
        };
        let s = map.entry(name).or_default();
        s.total_ns = s.total_ns.saturating_add(nanos);
        s.last_ns = nanos;
        s.count += 1;
        if nanos < s.min_ns {
            s.min_ns = nanos;
        }
        if nanos > s.max_ns {
            s.max_ns = nanos;
        }
    }

    /// `snapshot()` (`profiler.cpp:22-25`). A copy of the current aggregated
    /// buckets (owned `String` keys for the UI / CSV side).
    pub fn snapshot() -> Vec<(String, ProfileStats)> {
        let map = match state().stats.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        map.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
    }

    /// `reset()` (`profiler.cpp:27-30`). Clears every bucket to zero.
    pub fn reset() {
        let mut map = match state().stats.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        map.clear();
    }

    /// `dumpToStderr()` (`profiler.cpp`) — print a headless summary sorted by
    /// total time. This is useful to launch/test harnesses which cannot open
    /// the profiler dialog; callers decide when the capture is complete.
    pub fn dump_to_stderr() {
        let report = Self::format_report(Self::snapshot());
        let stderr = std::io::stderr();
        let mut stderr = stderr.lock();
        let _ = stderr.write_all(report.as_bytes());
        let _ = stderr.flush();
    }

    fn format_report(mut rows: Vec<(String, ProfileStats)>) -> String {
        rows.sort_by(|a, b| b.1.total_ns.cmp(&a.1.total_ns).then_with(|| a.0.cmp(&b.0)));

        let mut out = String::new();
        let _ = writeln!(
            out,
            "\n=== PROFILE (by total time, {} scopes) ===",
            rows.len()
        );
        let _ = writeln!(
            out,
            "{:<46} {:>10} {:>7} {:>10} {:>10}",
            "scope", "total_ms", "count", "avg_us", "max_us"
        );
        for (name, stats) in rows {
            let total_ms = stats.total_ns as f64 / 1_000_000.0;
            let avg_us = stats.mean_ns() / 1_000.0;
            let max_us = stats.max_ns as f64 / 1_000.0;
            let _ = writeln!(
                out,
                "{name:<46} {total_ms:>10.2} {:>7} {avg_us:>10.1} {max_us:>10.1}",
                stats.count
            );
        }
        out.push_str("=== END PROFILE ===\n");
        out
    }

    /// Number of distinct buckets currently recorded (test/diagnostic helper).
    pub fn bucket_count() -> usize {
        match state().stats.lock() {
            Ok(g) => g.len(),
            Err(p) => p.into_inner().len(),
        }
    }
}

/// RAII scoped timer — port of `class ProfileScope` (`profiler.h:57-72`).
///
/// Captures the enabled state at construction so a scope is "all or nothing":
/// flipping the global flag mid-scope does not retroactively start a sample and
/// does not abort one in flight. When inactive the `Drop` is a no-op. `name`
/// must be a `'static` string literal (captured by reference, no copy). One
/// sample per scope, recorded on drop.
pub struct ProfileScope {
    name: &'static str,
    start: Option<Instant>,
}

impl ProfileScope {
    /// Start a scope (`ProfileScope(const char* name)`). Reads the global
    /// enabled flag once; only starts the timer when enabled.
    pub fn new(name: &'static str) -> Self {
        let start = if Profiler::is_enabled() {
            Some(Instant::now())
        } else {
            None
        };
        ProfileScope { name, start }
    }
}

impl Drop for ProfileScope {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            let nanos = start.elapsed().as_nanos();
            // Clamp the 128-bit nanos into the u64 bucket (a single scope can
            // never realistically exceed ~584 years; saturate defensively).
            Profiler::record(self.name, nanos.min(u64::MAX as u128) as u64);
        }
    }
}

/// `PROFILE_SCOPE(literalName)` (`profiler.h:80-81`). Instruments the enclosing
/// block: declares an RAII [`ProfileScope`] bound to a unique local so multiple
/// uses in one function don't collide. `name` must be a string literal.
///
/// ```ignore
/// fn compose(&mut self) {
///     crate::PROFILE_SCOPE!("Compose::run");
///     // … hot work …
/// }
/// ```
#[macro_export]
macro_rules! PROFILE_SCOPE {
    ($name:literal) => {
        let _rcx_prof_scope = $crate::theme::profiler::ProfileScope::new($name);
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    // The profiler is a process-global singleton; serialize the tests that
    // mutate it so they don't interleave (cargo runs tests in parallel).
    static SERIAL: StdMutex<()> = StdMutex::new(());

    fn with_clean<R>(f: impl FnOnce() -> R) -> R {
        let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        Profiler::reset();
        Profiler::set_enabled(false);
        let r = f();
        Profiler::set_enabled(false);
        Profiler::reset();
        r
    }

    #[test]
    fn disabled_drops_samples() {
        with_clean(|| {
            Profiler::set_enabled(false);
            Profiler::record("x", 100);
            assert!(Profiler::snapshot().is_empty());
        });
    }

    #[test]
    fn enabled_aggregates_min_max_total_last_count() {
        with_clean(|| {
            Profiler::set_enabled(true);
            Profiler::record("f", 100);
            Profiler::record("f", 300);
            Profiler::record("f", 200);
            let snap = Profiler::snapshot();
            assert_eq!(snap.len(), 1);
            let (name, s) = &snap[0];
            assert_eq!(name, "f");
            assert_eq!(s.count, 3);
            assert_eq!(s.total_ns, 600);
            assert_eq!(s.min_ns, 100);
            assert_eq!(s.max_ns, 300);
            assert_eq!(s.last_ns, 200);
            assert_eq!(s.mean_ns(), 200.0);
        });
    }

    #[test]
    fn reset_clears_buckets() {
        with_clean(|| {
            Profiler::set_enabled(true);
            Profiler::record("a", 1);
            Profiler::record("b", 2);
            assert_eq!(Profiler::bucket_count(), 2);
            Profiler::reset();
            assert_eq!(Profiler::bucket_count(), 0);
            assert!(Profiler::snapshot().is_empty());
        });
    }

    #[test]
    fn stderr_report_is_sorted_by_total_time() {
        with_clean(|| {
            Profiler::set_enabled(true);
            Profiler::record("small", 1_000);
            Profiler::record("large", 5_000);

            let report = Profiler::format_report(Profiler::snapshot());
            assert!(report.contains("PROFILE (by total time, 2 scopes)"));
            assert!(report.find("large").unwrap() < report.find("small").unwrap());
            assert!(report.ends_with("=== END PROFILE ===\n"));
        });
    }

    #[test]
    fn scope_records_one_sample_when_enabled() {
        with_clean(|| {
            Profiler::set_enabled(true);
            {
                let _s = ProfileScope::new("scoped");
                // do a tiny bit of work so elapsed() > 0 on most platforms
                std::hint::black_box(0u64.wrapping_add(1));
            }
            let snap = Profiler::snapshot();
            assert_eq!(snap.len(), 1);
            assert_eq!(snap[0].0, "scoped");
            assert_eq!(snap[0].1.count, 1);
        });
    }

    #[test]
    fn scope_is_all_or_nothing_on_flag_flip() {
        with_clean(|| {
            // Disabled at construction → no sample even if enabled before drop.
            Profiler::set_enabled(false);
            {
                let _s = ProfileScope::new("late_enable");
                Profiler::set_enabled(true);
            }
            // The scope captured "disabled" at construction → dropped.
            assert!(Profiler::snapshot().is_empty());
        });
    }

    #[test]
    fn macro_instruments_block() {
        with_clean(|| {
            Profiler::set_enabled(true);
            fn work() {
                crate::PROFILE_SCOPE!("macro_block");
                std::hint::black_box(1u64);
            }
            work();
            let snap = Profiler::snapshot();
            assert_eq!(snap.len(), 1);
            assert_eq!(snap[0].0, "macro_block");
        });
    }

    #[test]
    fn min_ns_display_is_zero_before_first_sample() {
        let s = ProfileStats::default();
        assert_eq!(s.min_ns_display(), 0);
        assert_eq!(s.mean_ns(), 0.0);
    }
}
