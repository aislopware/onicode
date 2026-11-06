//! What the Mac's shared media and graphics blocks did over a span, read without root.
//!
//! `powermetrics` needs root, and a measurement run from a test has none. The counters it reads
//! come from `IOReport` (`libIOReport.dylib`), which any process may subscribe to. Over a span this
//! reads, system-wide:
//!
//! - each hardware encode engine's interrupts (`Interrupt Statistics (by index)`, `ave0 0` and
//!   `ave1 0`): one or more a coded frame, so which engine a session's frames went to;
//! - each engine's DRAM traffic (`AMC Stats`, `VENC0` and `VENC1` reads and writes);
//! - how long the GPU was out of its `OFF` state, and the energy it spent (`GPU Stats`, `GPUPH`;
//!   `Energy Model`, `GPU Energy`).
//!
//! The encode block's own energy (`Energy Model`, `AVE0`) reads 0 mJ over any span on an M1 Max
//! running macOS 27.0, and its power state (`SoC Stats`, `AVEMSR`) is `ACT` all the time, idle
//! or not, so neither is offered. Everything here counts every process on the Mac:
//! read an idle span first and take it off. Off macOS, [`Soc::open`] is `None`.

use std::time::Duration;

/// What the counters moved by between two readings ([`Reading::since`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Span {
    /// The wall time between the readings.
    pub elapsed: Duration,
    /// Interrupts each encode engine raised, `ave0` and `ave1`.
    pub encode_interrupts: [u64; 2],
    /// Bytes each encode engine read and wrote in DRAM, `VENC0` and `VENC1`.
    pub encode_bytes: [u64; 2],
    /// The share of the span the GPU was out of its `OFF` state, 0 to 1.
    pub gpu_active: f64,
    /// The energy the GPU spent, joules.
    pub gpu_joules: f64,
}

impl Span {
    /// Per second, this span less `idle`'s rate over the same length: what a measured load added
    /// to what the Mac was doing anyway. Shares and counts floor at 0.
    #[must_use]
    pub fn less(&self, idle: &Self) -> Rates {
        let secs = self.elapsed.as_secs_f64().max(f64::EPSILON);
        let idle_secs = idle.elapsed.as_secs_f64().max(f64::EPSILON);
        #[expect(clippy::cast_precision_loss, reason = "counts far below 2^52")]
        let rate = |n: u64, m: u64| (n as f64 / secs - m as f64 / idle_secs).max(0.0);
        Rates {
            encode_interrupts: [
                rate(self.encode_interrupts[0], idle.encode_interrupts[0]),
                rate(self.encode_interrupts[1], idle.encode_interrupts[1]),
            ],
            encode_bytes: [
                rate(self.encode_bytes[0], idle.encode_bytes[0]),
                rate(self.encode_bytes[1], idle.encode_bytes[1]),
            ],
            gpu_active: (self.gpu_active - idle.gpu_active).max(0.0),
            gpu_watts: (self.gpu_joules / secs - idle.gpu_joules / idle_secs).max(0.0),
        }
    }
}

/// A [`Span`] per second, less an idle span ([`Span::less`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rates {
    /// Interrupts a second on `ave0` and `ave1`.
    pub encode_interrupts: [f64; 2],
    /// DRAM bytes a second of `VENC0` and `VENC1`.
    pub encode_bytes: [f64; 2],
    /// The share of time the GPU was active, above idle.
    pub gpu_active: f64,
    /// GPU watts above idle.
    pub gpu_watts: f64,
}

pub use imp::{Reading, Soc};

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::ptr::{self, NonNull};
    use std::time::Instant;

    use objc2_core_foundation::{
        CFArray, CFDictionary, CFMutableDictionary, CFRetained, CFString, CFType,
    };

    use super::Span;

    /// `IOReportSubscriptionRef`: a CF object `IOReport` hands out and `CFRelease` frees.
    type Subscription = CFType;

    // IOReport is private: `libIOReport.dylib` exports these C functions and no SDK header
    // declares them. The signatures are the ones every root-free power monitor calls them with
    // (macmon, asitop's readers): CF objects in, `Create`/`Copy` results owned (+1), `Get`
    // results borrowed (+0) from the channel they came from.
    #[link(name = "IOReport", kind = "dylib")]
    unsafe extern "C-unwind" {
        fn IOReportCopyChannelsInGroup(
            group: &CFString,
            subgroup: Option<&CFString>,
            a: u64,
            b: u64,
            c: u64,
        ) -> Option<NonNull<CFDictionary>>;
        fn IOReportMergeChannels(into: &CFDictionary, from: &CFDictionary, nil: *const c_void);
        fn IOReportCreateSubscription(
            nil: *const c_void,
            desired: &CFMutableDictionary,
            subscribed: *mut *mut CFMutableDictionary,
            channel_id: u64,
            nil2: *const c_void,
        ) -> Option<NonNull<Subscription>>;
        fn IOReportCreateSamples(
            subscription: &Subscription,
            subscribed: &CFMutableDictionary,
            nil: *const c_void,
        ) -> Option<NonNull<CFDictionary>>;
        fn IOReportCreateSamplesDelta(
            before: &CFDictionary,
            after: &CFDictionary,
            nil: *const c_void,
        ) -> Option<NonNull<CFDictionary>>;
        fn IOReportChannelGetGroup(channel: &CFDictionary) -> Option<NonNull<CFString>>;
        fn IOReportChannelGetSubGroup(channel: &CFDictionary) -> Option<NonNull<CFString>>;
        fn IOReportChannelGetChannelName(channel: &CFDictionary) -> Option<NonNull<CFString>>;
        fn IOReportChannelGetUnitLabel(channel: &CFDictionary) -> Option<NonNull<CFString>>;
        fn IOReportSimpleGetIntegerValue(channel: &CFDictionary, index: i32) -> i64;
        fn IOReportStateGetCount(channel: &CFDictionary) -> i32;
        fn IOReportStateGetNameForIndex(
            channel: &CFDictionary,
            index: i32,
        ) -> Option<NonNull<CFString>>;
        fn IOReportStateGetResidency(channel: &CFDictionary, index: i32) -> i64;
    }

    /// The channels read, by group and subgroup (`None`: the whole group).
    const CHANNELS: [(&str, Option<&str>); 5] = [
        ("Interrupt Statistics (by index)", Some("ave0 0")),
        ("Interrupt Statistics (by index)", Some("ave1 0")),
        ("AMC Stats", Some("Perf Counters")),
        ("GPU Stats", Some("GPU Performance States")),
        ("Energy Model", None),
    ];

    /// A subscription to the counters [`Span`] reports.
    pub struct Soc {
        subscription: CFRetained<Subscription>,
        subscribed: CFRetained<CFMutableDictionary>,
    }

    /// The counters at one instant ([`Soc::read`]).
    pub struct Reading {
        at: Instant,
        samples: CFRetained<CFDictionary>,
    }

    impl std::fmt::Debug for Soc {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Soc").finish_non_exhaustive()
        }
    }

    impl std::fmt::Debug for Reading {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Reading")
                .field("at", &self.at)
                .finish_non_exhaustive()
        }
    }

    /// A `+0` string `IOReport` lent out of a channel, as text.
    fn text(borrowed: Option<NonNull<CFString>>) -> String {
        // SAFETY: an IOReport `Get` result is a valid `CFString` borrowed from the channel,
        // which the caller holds for the whole call.
        borrowed
            .map(|s| unsafe { s.as_ref() }.to_string().trim().to_owned())
            .unwrap_or_default()
    }

    impl Soc {
        /// Subscribe; `None` when `IOReport` offers none of the channels.
        #[must_use]
        pub fn open() -> Option<Self> {
            let mut merged: Option<CFRetained<CFDictionary>> = None;
            for (group, subgroup) in CHANNELS {
                let group = CFString::from_str(group);
                let subgroup = subgroup.map(CFString::from_str);
                // SAFETY: IOReport's `Copy` rule: the dictionary returned is owned (+1).
                let Some(found) =
                    (unsafe { IOReportCopyChannelsInGroup(&group, subgroup.as_deref(), 0, 0, 0) })
                else {
                    continue;
                };
                // SAFETY: as above, a +1 `CFDictionary` now owned here.
                let found = unsafe { CFRetained::from_raw(found) };
                match &merged {
                    // SAFETY: both are channel dictionaries from `IOReportCopyChannelsInGroup`;
                    // the merge appends `found`'s channels to `into` in place.
                    Some(into) => unsafe { IOReportMergeChannels(into, &found, ptr::null()) },
                    None => merged = Some(found),
                }
            }
            let merged = merged?;
            // SAFETY: a mutable copy of a CF dictionary of CF values; no generics are read back
            // through it.
            let desired = unsafe { CFMutableDictionary::new_copy(None, 0, Some(&merged)) }?;
            let mut subscribed: *mut CFMutableDictionary = ptr::null_mut();
            // SAFETY: IOReport's `Create` rule: the subscription and the dictionary written to
            // `subscribed` are owned (+1); the out-pointer is valid for the call.
            let subscription = unsafe {
                IOReportCreateSubscription(
                    ptr::null(),
                    &desired,
                    &raw mut subscribed,
                    0,
                    ptr::null(),
                )
            }?;
            // SAFETY: owned (+1), as above.
            let subscription = unsafe { CFRetained::from_raw(subscription) };
            // SAFETY: owned (+1), as above; checked non-null.
            let subscribed = unsafe { CFRetained::from_raw(NonNull::new(subscribed)?) };
            Some(Self {
                subscription,
                subscribed,
            })
        }

        /// The counters now.
        #[must_use]
        pub fn read(&self) -> Option<Reading> {
            // SAFETY: the subscription and its channels are the pair `open` made; the samples
            // come back owned (+1).
            let samples = unsafe {
                IOReportCreateSamples(&self.subscription, &self.subscribed, ptr::null())
            }?;
            // SAFETY: owned (+1), as above.
            let samples = unsafe { CFRetained::from_raw(samples) };
            Some(Reading {
                at: Instant::now(),
                samples,
            })
        }
    }

    impl Reading {
        /// What the counters moved by from `before` to this reading, both of one [`Soc`].
        #[must_use]
        pub fn since(&self, before: &Self) -> Option<Span> {
            let after = self;
            // SAFETY: two samples of this subscription; the delta comes back owned (+1).
            let delta = unsafe {
                IOReportCreateSamplesDelta(&before.samples, &after.samples, ptr::null())
            }?;
            // SAFETY: owned (+1), as above.
            let delta = unsafe { CFRetained::from_raw(delta) };
            // SAFETY: IOReport's samples are a dictionary whose `IOReportChannels` key holds an
            // array of channel dictionaries; both reads below go through `get`, which checks
            // nothing, so the casts state that layout.
            let delta: CFRetained<CFDictionary<CFString, CFArray<CFDictionary>>> =
                unsafe { CFRetained::cast_unchecked(delta) };
            let channels = delta.get(&CFString::from_str("IOReportChannels"))?;
            let mut span = Span {
                elapsed: after.at.duration_since(before.at),
                ..Span::default()
            };
            for i in 0..channels.len() {
                let Some(channel) = channels.get(i) else {
                    continue;
                };
                read_channel(&channel, &mut span);
            }
            Some(span)
        }
    }

    /// Add what one channel of a delta says to `span`.
    fn read_channel(channel: &CFDictionary, span: &mut Span) {
        let group = text(
            // SAFETY: `channel` is a channel dictionary of a delta this process owns, held for
            // the whole call; the name is borrowed from it.
            unsafe { IOReportChannelGetGroup(channel) },
        );
        // SAFETY: as above.
        let subgroup = text(unsafe { IOReportChannelGetSubGroup(channel) });
        // SAFETY: as above.
        let name = text(unsafe { IOReportChannelGetChannelName(channel) });
        let simple = || {
            // SAFETY: as above; index 0 is a simple channel's one value.
            u64::try_from(unsafe { IOReportSimpleGetIntegerValue(channel, 0) }).ok()
        };
        let engine = |s: &str| match s {
            "ave0 0" | "VENC0" => Some(0_usize),
            "ave1 0" | "VENC1" => Some(1),
            _ => None,
        };
        let add = |slot: Option<&mut u64>, n: u64| {
            if let Some(slot) = slot {
                *slot = slot.saturating_add(n);
            }
        };
        match group.as_str() {
            "Interrupt Statistics (by index)" if name == "First Level Interrupt Handler Count" => {
                if let (Some(e), Some(n)) = (engine(&subgroup), simple()) {
                    add(span.encode_interrupts.get_mut(e), n);
                }
            }
            // Plain reads and writes; the `DCS` counters count the same traffic again.
            "AMC Stats"
                if (name.ends_with(" RD") || name.ends_with(" WR")) && !name.contains("DCS") =>
            {
                let unit = name.split(' ').next().unwrap_or_default();
                if let (Some(e), Some(n)) = (engine(unit), simple()) {
                    add(span.encode_bytes.get_mut(e), n);
                }
            }
            "GPU Stats" if name == "GPUPH" => {
                span.gpu_active = share(channel, |state| state != "OFF");
            }
            "Energy Model" if name == "GPU Energy" => {
                // SAFETY: as above.
                let unit = text(unsafe { IOReportChannelGetUnitLabel(channel) });
                let per_joule = match unit.as_str() {
                    "mJ" => 1e3,
                    "uJ" | "\u{b5}J" => 1e6,
                    _ => 1e9,
                };
                #[expect(clippy::cast_precision_loss, reason = "energy counts far below 2^52")]
                if let Some(n) = simple() {
                    span.gpu_joules += n as f64 / per_joule;
                }
            }
            _ => {}
        }
    }

    /// The share of a state channel's residency spent in the states `counts` picks.
    fn share(channel: &CFDictionary, counts: impl Fn(&str) -> bool) -> f64 {
        let (mut picked, mut all) = (0_i64, 0_i64);
        // SAFETY: as in `read_channel`: a channel of an owned delta, `Get` results borrowed.
        let states = unsafe { IOReportStateGetCount(channel) };
        for i in 0..states {
            // SAFETY: as above, `i` within the count IOReport gave.
            let residency = unsafe { IOReportStateGetResidency(channel, i) }.max(0);
            // SAFETY: as above.
            let state = text(unsafe { IOReportStateGetNameForIndex(channel, i) });
            all = all.saturating_add(residency);
            if counts(&state) {
                picked = picked.saturating_add(residency);
            }
        }
        #[expect(clippy::cast_precision_loss, reason = "residency ticks far below 2^52")]
        if all > 0 {
            picked as f64 / all as f64
        } else {
            0.0
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Span;

    /// No `IOReport` off macOS.
    #[derive(Debug)]
    #[expect(
        missing_copy_implementations,
        reason = "the same type as on macOS, where it holds IOReport handles"
    )]
    pub struct Soc;

    /// No `IOReport` off macOS.
    #[derive(Debug)]
    #[expect(
        missing_copy_implementations,
        reason = "the same type as on macOS, where it holds IOReport handles"
    )]
    pub struct Reading;

    impl Soc {
        /// Always `None` off macOS.
        #[must_use]
        pub const fn open() -> Option<Self> {
            None
        }

        /// Never reached: there is no [`Soc`] off macOS.
        #[must_use]
        pub const fn read(&self) -> Option<Reading> {
            None
        }
    }

    impl Reading {
        /// Never reached: there is no [`Soc`] off macOS.
        #[must_use]
        pub const fn since(&self, _before: &Self) -> Option<Span> {
            None
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;

    /// The counters answer on this Mac, and a span of nothing measured reads as a span: the
    /// encode block and the GPU are shares of it, and the elapsed time is the one waited.
    #[test]
    fn a_span_reads_back() {
        // A virtual Mac (the office CI runners) has no SoC counters to subscribe to.
        let Some(soc) = Soc::open() else {
            crate::live::skip("IOReport has no SoC counters on this Mac (a virtual machine)");
            return;
        };
        let (Some(before), Some(after)) = (soc.read(), {
            std::thread::sleep(Duration::from_millis(200));
            soc.read()
        }) else {
            crate::live::skip("IOReport subscribed but reads nothing on this Mac");
            return;
        };
        let span = after.since(&before).expect("a delta");
        assert!(span.elapsed >= Duration::from_millis(200), "{span:?}");
        assert!((0.0..=1.0).contains(&span.gpu_active), "{span:?}");
        assert!(span.gpu_joules >= 0.0, "{span:?}");
        let rates = span.less(&span);
        assert!(
            rates.encode_interrupts.iter().all(|&r| r.abs() < 1e-6),
            "{rates:?}"
        );
    }
}
