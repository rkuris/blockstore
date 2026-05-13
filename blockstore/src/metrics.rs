//! Conditional metrics shim.
//!
//! With `feature = "metrics"` enabled, `counter!` and `record_duration!`
//! delegate to the [`metrics`] crate. Otherwise they expand to no-ops so
//! the rest of the crate doesn't have to `#[cfg]`-gate every call site.
//!
//! Both forms of the underlying `metrics::counter!` are supported —
//! plain and tagged with `"label" => "value"` pairs. Tagged counters
//! follow the same convention as firewood (e.g. one counter named
//! `cache.read` with `type=hit|miss`), which keeps the metric surface
//! compact at query time.

// `#[macro_use] mod metrics;` in `lib.rs` exposes these macros to every
// later sibling module. Both define a `counter!` and `record_duration!`
// macro — when the `metrics` feature is on they delegate to the
// `metrics` crate; otherwise they expand to no-ops.

#[cfg(feature = "metrics")]
macro_rules! counter {
    ($($args:tt)*) => {
        ::metrics::counter!($($args)*)
    };
}

#[cfg(not(feature = "metrics"))]
macro_rules! counter {
    ($key:expr $(, $label:expr => $value:expr)* $(,)?) => {{
        struct FakeCounter;
        impl FakeCounter {
            pub fn increment(&self, _: u64) {}
        }
        // Reference unused args so the macro doesn't warn about them.
        $( let _ = $label; let _ = $value; )*
        let _ = $key;
        FakeCounter {}
    }};
}

#[cfg(feature = "metrics")]
macro_rules! record_duration {
    ($start:expr, $key:expr) => {
        let duration = coarsetime::Instant::now().duration_since($start);
        counter!($key).increment(duration.as_millis() as u64);
    };
}

#[cfg(not(feature = "metrics"))]
macro_rules! record_duration {
    ($start:expr, $key:expr) => {
        // do nothing
    };
}
