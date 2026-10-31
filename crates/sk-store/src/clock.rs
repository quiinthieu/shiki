//! Production `Clock` implementation backed by `std::time::SystemTime`.

use sk_core::time::{Clock, UtcInstant};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Copy, Clone, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UtcInstant {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(dur) => UtcInstant::from_unix_secs(i64::try_from(dur.as_secs()).unwrap_or(i64::MAX)),
            Err(err) => {
                let dur = err.duration();
                let secs = i64::try_from(dur.as_secs()).unwrap_or(i64::MAX);
                let subsec_adjust = i64::from(dur.subsec_nanos() > 0);
                UtcInstant::from_unix_secs(-secs.saturating_add(subsec_adjust))
            }
        }
    }
}
