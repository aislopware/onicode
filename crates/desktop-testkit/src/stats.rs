//! Percentiles of a measurement's samples: the one helper the `*_cost` tests share.

use std::time::Duration;

/// Where a sample set lies: its size, extremes and percentiles, in the samples' unit.
///
/// A percentile is the nearest-rank one: the smallest sample at least `q` of the set are no
/// larger than.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spread {
    /// Samples.
    pub n: usize,
    /// The smallest.
    pub min: u64,
    /// Median.
    pub p50: u64,
    /// 90th percentile.
    pub p90: u64,
    /// 95th percentile.
    pub p95: u64,
    /// 99th percentile.
    pub p99: u64,
    /// 99.9th percentile.
    pub p999: u64,
    /// The largest.
    pub max: u64,
}

impl Spread {
    /// The spread of `samples`, which it sorts; `None` when there are none.
    pub fn of(samples: &mut [u64]) -> Option<Self> {
        samples.sort_unstable();
        let sorted: &[u64] = samples;
        Some(Self {
            n: sorted.len(),
            min: *sorted.first()?,
            p50: percentile(sorted, 500)?,
            p90: percentile(sorted, 900)?,
            p95: percentile(sorted, 950)?,
            p99: percentile(sorted, 990)?,
            p999: percentile(sorted, 999)?,
            max: *sorted.last()?,
        })
    }

    /// The spread of `samples` in nanoseconds.
    #[must_use]
    pub fn of_durations(samples: &[Duration]) -> Option<Self> {
        let mut ns: Vec<u64> = samples
            .iter()
            .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
            .collect();
        Self::of(&mut ns)
    }

    /// Every value divided by `by` (samples that each covered `by` operations); `by` of zero
    /// leaves it as it is.
    #[must_use]
    pub fn per(self, by: u64) -> Self {
        let d = |v: u64| v.checked_div(by).unwrap_or(v);
        Self {
            n: self.n,
            min: d(self.min),
            p50: d(self.p50),
            p90: d(self.p90),
            p95: d(self.p95),
            p99: d(self.p99),
            p999: d(self.p999),
            max: d(self.max),
        }
    }
}

impl std::fmt::Display for Spread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "p50 {} p95 {} p99 {} max {} (n {})",
            self.p50, self.p95, self.p99, self.max, self.n
        )
    }
}

/// The nearest-rank percentile `permille`/1000 of `sorted`.
fn percentile(sorted: &[u64], permille: usize) -> Option<u64> {
    let rank = sorted.len().saturating_mul(permille).div_ceil(1000).max(1);
    sorted.get(rank.saturating_sub(1)).copied()
}

/// Nanoseconds as microseconds with one decimal, for the printed lines.
#[must_use]
pub fn us(ns: u64) -> String {
    format!("{}.{} us", ns / 1_000, (ns % 1_000) / 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let mut v: Vec<u64> = (1..=100).rev().collect();
        let s = Spread::of(&mut v).unwrap();
        assert_eq!(
            (s.n, s.min, s.p50, s.p90, s.p95, s.p99, s.max),
            (100, 1, 50, 90, 95, 99, 100),
            "{s:?}"
        );
        let one = Spread::of(&mut [7]).unwrap();
        assert_eq!((one.p50, one.p99, one.max), (7, 7, 7), "{one:?}");
        assert_eq!(Spread::of(&mut []), None, "no samples, no spread");
    }

    #[test]
    fn per_operation_and_durations() {
        let s = Spread::of_durations(&[Duration::from_micros(3), Duration::from_micros(1)]);
        let s = s.unwrap();
        assert_eq!((s.min, s.max), (1_000, 3_000), "{s:?}");
        assert_eq!(s.per(1_000).max, 3, "{s:?}");
        assert_eq!(s.per(0), s, "zero leaves it");
        assert_eq!(us(12_345), "12.3 us");
    }
}
