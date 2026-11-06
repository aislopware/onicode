//! A measurement's samples, and the one line each writes for a bench budget.
//!
//! A `*_cost` test opens a [`Bench`], takes a [`Series`] per thing it times, runs each sample
//! through [`Series::time`] and ends with [`Series::report`]. Each sample is timed and counted
//! in retired instructions ([`crate::process::instructions`]), less what the two readings
//! themselves cost, measured once when the bench opens.
//!
//! The report prints a line for a person and, when `ONICODE_BENCH_OUT` names a file, appends one
//! JSON object to it:
//!
//! ```text
//! {"name":"engine.frame_cost.take_frame","samples":1000,"ops":1,"instructions":41000,
//!  "wall_ns":{"p50":9000,"p95":12000,"p99":20000,"p999":40000,"max":61000}}
//! ```
//!
//! `instructions` is the median per operation, the number a bench budget
//! holds (it moves by well under a percent on a loaded machine). Wall time is recorded as it
//! came; only the nightly run keeps it, as a trend.
//!
//! The instruction count is the whole process's, so a series that runs other threads while it
//! is timed is [`Series::wall_only`].

use std::fmt::Write as _;
use std::io::Write as _;
use std::time::{Duration, Instant};

use crate::process;
use crate::stats::{Spread, us};

/// The variable naming the file the reports are appended to.
pub const OUT_ENV: &str = "ONICODE_BENCH_OUT";

/// Empty samples taken to learn what a sample's own readings cost.
const CALIBRATION: usize = 201;

/// One measurement: a name, and the cost of reading the counters around a sample.
#[derive(Debug, Clone)]
pub struct Bench {
    name: String,
    overhead: u64,
}

impl Bench {
    /// A measurement called `name` (`<crate>.<test>`, as the budget file keys it).
    #[must_use]
    pub fn new(name: &str) -> Self {
        let mut probe = Series {
            name: String::new(),
            overhead: 0,
            ops: 1,
            counted: true,
            samples: Vec::new(),
        };
        for _ in 0..CALIBRATION {
            probe.time(|| ());
        }
        let mut empty: Vec<u64> = probe
            .samples
            .iter()
            .filter_map(|s| s.instructions)
            .collect();
        let overhead = Spread::of(&mut empty).map_or(0, |s| s.p50);
        Self {
            name: name.to_owned(),
            overhead,
        }
    }

    /// A series of samples of `metric`, reported as `<bench>.<metric>`.
    #[must_use]
    pub fn series(&self, metric: &str) -> Series {
        Series {
            name: format!("{}.{metric}", self.name),
            overhead: self.overhead,
            ops: 1,
            counted: true,
            samples: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    wall: Duration,
    instructions: Option<u64>,
}

/// The samples of one thing a measurement times.
#[derive(Debug, Clone)]
pub struct Series {
    name: String,
    overhead: u64,
    ops: u64,
    counted: bool,
    samples: Vec<Sample>,
}

impl Series {
    /// Each sample covers `ops` operations, and is reported per operation.
    #[must_use]
    pub const fn ops(mut self, ops: u64) -> Self {
        self.ops = ops;
        self
    }

    /// Record no instruction count: the series runs other threads while it is timed, and the
    /// process's count would be theirs too.
    #[must_use]
    pub const fn wall_only(mut self) -> Self {
        self.counted = false;
        self
    }

    /// Run `f` as one sample.
    pub fn time<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let before = if self.counted {
            process::instructions()
        } else {
            None
        };
        let started = Instant::now();
        let out = f();
        let wall = started.elapsed();
        let after = if self.counted {
            process::instructions()
        } else {
            None
        };
        let instructions = before
            .zip(after)
            .map(|(b, a)| a.saturating_sub(b).saturating_sub(self.overhead));
        self.samples.push(Sample { wall, instructions });
        out
    }

    /// A sample timed by the caller: wall time only.
    pub fn record(&mut self, wall: Duration) {
        self.samples.push(Sample {
            wall,
            instructions: None,
        });
    }

    /// Print the series and append it to the file `ONICODE_BENCH_OUT` names; what it found.
    ///
    /// # Errors
    ///
    /// When there are no samples, or the file cannot be written.
    #[expect(
        clippy::print_stderr,
        reason = "a measurement's report is for the person running it"
    )]
    pub fn report(self) -> std::io::Result<Report> {
        let wall: Vec<Duration> = self.samples.iter().map(|s| s.wall).collect();
        let wall = Spread::of_durations(&wall)
            .ok_or_else(|| std::io::Error::other(format!("{}: no samples", self.name)))?
            .per(self.ops);
        let mut counted: Vec<u64> = self.samples.iter().filter_map(|s| s.instructions).collect();
        let instructions = Spread::of(&mut counted).map(|s| s.per(self.ops));
        let report = Report {
            name: self.name,
            samples: wall.n,
            ops: self.ops,
            wall,
            instructions,
        };
        eprintln!("{report}");
        if let Some(path) = std::env::var_os(OUT_ENV) {
            let mut line = report.json();
            line.push('\n');
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?
                .write_all(line.as_bytes())?;
        }
        Ok(report)
    }
}

/// What a series found, per operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// `<crate>.<test>.<metric>`.
    pub name: String,
    /// Samples taken.
    pub samples: usize,
    /// Operations per sample.
    pub ops: u64,
    /// Wall time, nanoseconds.
    pub wall: Spread,
    /// Retired instructions, when they were counted.
    pub instructions: Option<Spread>,
}

impl Report {
    /// The report as one JSON object.
    #[must_use]
    pub fn json(&self) -> String {
        let instructions = self
            .instructions
            .map_or_else(|| "null".to_owned(), |s| s.p50.to_string());
        format!(
            "{{\"name\":\"{}\",\"samples\":{},\"ops\":{},\"instructions\":{instructions},\
             \"wall_ns\":{{\"p50\":{},\"p95\":{},\"p99\":{},\"p999\":{},\"max\":{}}}}}",
            escaped(&self.name),
            self.samples,
            self.ops,
            self.wall.p50,
            self.wall.p95,
            self.wall.p99,
            self.wall.p999,
            self.wall.max,
        )
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BENCH {}:", self.name)?;
        if let Some(i) = self.instructions {
            write!(f, " {} instructions/op (p95 {}),", i.p50, i.p95)?;
        }
        write!(
            f,
            " wall p50 {} p95 {} p99 {} max {} (n {}",
            us(self.wall.p50),
            us(self.wall.p95),
            us(self.wall.p99),
            us(self.wall.max),
            self.samples
        )?;
        if self.ops > 1 {
            write!(f, " x {} ops", self.ops)?;
        }
        write!(f, ")")
    }
}

/// `s` as the inside of a JSON string.
fn escaped(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => {
                let _written = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_series_reports_per_operation_and_as_json() {
        let bench = Bench::new("testkit.self");
        let mut s = bench.series("spin").ops(1_000);
        for _ in 0..20 {
            s.time(|| {
                let mut x = 0_u64;
                for i in 0..1_000_u64 {
                    x = std::hint::black_box(x.wrapping_add(i));
                }
                x
            });
        }
        let r = s.report().unwrap();
        assert_eq!(
            (r.name.as_str(), r.samples, r.ops),
            ("testkit.self.spin", 20, 1_000)
        );
        #[cfg(target_os = "macos")]
        if let Some(i) = r.instructions.filter(|i| i.max > 0) {
            assert!(
                (1..100).contains(&i.p50),
                "a few instructions per addition: {i:?}"
            );
        }
        let json = r.json();
        assert!(json.starts_with("{\"name\":\"testkit.self.spin\",\"samples\":20,\"ops\":1000,"));
        assert!(json.ends_with("}}"), "{json}");
    }

    #[test]
    fn no_samples_is_an_error_and_names_are_escaped() {
        let s = Bench::new("testkit.self").series("none").wall_only();
        assert!(s.report().is_err(), "nothing to report");
        assert_eq!(escaped("a\"b\\c\n"), "a\\\"b\\\\c\\u000a");
    }
}
