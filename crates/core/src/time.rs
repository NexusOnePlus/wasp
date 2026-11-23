//! The one place the engine's timestamp type becomes the domain's clock.
//!
//! `whatsapp-rust` moved from `chrono::DateTime<Utc>` to its own
//! millisecond-precision [`wacore::Timestamp`]. Domain types still carry
//! `DateTime<Utc>` because the interface formats and compares calendar
//! values; this module is the border between the two.

use chrono::{DateTime, Utc};

/// the current instant, in the domain's clock type.
pub fn now() -> DateTime<Utc> {
    from_wacore(wacore::time::now_utc())
}

/// Convert an engine timestamp into the domain's clock type.
///
/// `wacore` validates its range to instants `chrono` can represent, so this
/// cannot fail for a timestamp the engine produced.
pub fn from_wacore(timestamp: wacore::Timestamp) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(timestamp.as_millis())
        .expect("a wacore timestamp is representable in chrono")
}
