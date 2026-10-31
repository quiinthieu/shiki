use sk_core::time::{Clock, UtcInstant};
use sk_store::clock::SystemClock;

#[test]
fn system_clock_returns_plausible_monotonic_utc_instant() {
    let clock = SystemClock;
    let first = clock.now();
    let second = clock.now();

    assert!(
        first >= UtcInstant::utc_ymd_hms(2025, 1, 1, 0, 0, 0),
        "SystemClock::now() returned pre-2025 instant: {first}"
    );
    assert!(
        second.duration_since(first) >= -1,
        "back-to-back SystemClock::now() calls regressed unexpectedly: {first} -> {second}"
    );
}
