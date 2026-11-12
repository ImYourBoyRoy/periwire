//! Reconnect grace period (`FLAG_RESUME`).
//!
//! When a connection to an importer that asked for it ends abruptly (the network dropped, the other computer crashed),
//! the exporter does not hand the drive back to its own desktop at once. The desktop would mount it read-write within
//! a second, and the exporter refuses to share a mounted drive, so the importer's automatic reconnect would be turned
//! away until somebody unmounted it. Instead the exporter keeps the device claimed ("parked") for a grace period, and
//! only the same peer, asking for the same options, can pick it up again.
//!
//! This module holds the parts that need no hardware: how a session ended, whether that earns a grace period, and the
//! parking lot itself (generic over what is parked, so the tests park plain integers).
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a lost session's device is held for its importer (`PERIWIRE_RECONNECT_GRACE` seconds, `0` turns it off).
pub const DEFAULT_GRACE_SECS: u64 = 60;

pub fn grace() -> Duration {
    let secs = std::env::var("PERIWIRE_RECONNECT_GRACE").ok().and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(DEFAULT_GRACE_SECS);
    Duration::from_secs(secs.min(3600))
}

/// How a session's connection ended.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Cause {
    /// The importer closed the connection properly, the operator disconnected it, or the exporter is stopping.
    Clean,
    /// The connection died without a proper close (reset, timeout, no TLS close_notify), or the same peer came back on a
    /// new connection while the old one had not been noticed as dead yet.
    Lost,
    /// A protocol error: the other side sent something invalid. Nothing worth resuming.
    Failed,
}

/// Does this ending earn a grace period? Only for importers that asked (`FLAG_RESUME`), only after a loss, and only if
/// the grace period is not switched off.
pub fn should_park(cause: Cause, resume: bool, grace: Duration) -> bool {
    resume && cause == Cause::Lost && !grace.is_zero()
}

/// What `take` found for a device.
#[derive(Debug, PartialEq, Eq)]
pub enum Taken<T> {
    /// Held for exactly this peer and these options: use it.
    Resumed(T),
    /// Held for a different peer, and its time has not run out: refuse the newcomer.
    Reserved,
    /// Held for this peer but with different options, or already expired: it was removed; release it and open normally.
    Stale(T),
    Nothing,
}

struct Slot<T> {
    item: T,
    peer: String,
    options: u32,
    deadline: Instant,
    id: u64,
}

/// Devices waiting for their importer to come back, by bus id.
pub struct Parking<T> {
    slots: HashMap<String, Slot<T>>,
    next: u64,
}

impl<T> Default for Parking<T> {
    fn default() -> Self {
        Parking { slots: HashMap::new(), next: 1 }
    }
}

impl<T> Parking<T> {
    /// Hold `item` for `peer` (a certificate fingerprint) until `deadline`. `options` are the session flags that must
    /// match for it to be resumed. Returns the slot's id (for `expire`) and any item it displaced.
    pub fn park(&mut self, busid: &str, peer: &str, options: u32, item: T, deadline: Instant) -> (u64, Option<T>) {
        let id = self.next;
        self.next += 1;
        let old = self.slots.insert(busid.to_string(), Slot { item, peer: peer.to_string(), options, deadline, id });
        (id, old.map(|s| s.item))
    }

    pub fn take(&mut self, busid: &str, peer: &str, options: u32, now: Instant) -> Taken<T> {
        let Some(slot) = self.slots.get(busid) else { return Taken::Nothing };
        let expired = now >= slot.deadline;
        if slot.peer != peer && !expired {
            return Taken::Reserved;
        }
        let slot = self.slots.remove(busid).expect("slot was just seen");
        if slot.peer == peer && slot.options == options && !expired {
            Taken::Resumed(slot.item)
        } else {
            Taken::Stale(slot.item)
        }
    }

    /// Remove the slot if it is still the one `park` returned `id` for (a later park of the same device keeps its own timer).
    pub fn expire(&mut self, busid: &str, id: u64) -> Option<T> {
        if self.slots.get(busid).map(|s| s.id) == Some(id) {
            self.slots.remove(busid).map(|s| s.item)
        } else {
            None
        }
    }

    pub fn remove(&mut self, busid: &str) -> Option<T> {
        self.slots.remove(busid).map(|s| s.item)
    }

    pub fn contains(&self, busid: &str) -> bool {
        self.slots.contains_key(busid)
    }

    pub fn drain(&mut self) -> Vec<T> {
        self.slots.drain().map(|(_, s)| s.item).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: Duration = Duration::from_secs(60);

    #[test]
    fn only_a_lost_connection_of_an_asking_importer_is_held() {
        assert!(should_park(Cause::Lost, true, G));
        assert!(!should_park(Cause::Lost, false, G), "an importer that did not ask gets the old behaviour");
        assert!(!should_park(Cause::Clean, true, G), "a deliberate detach returns the drive at once");
        assert!(!should_park(Cause::Failed, true, G));
        assert!(!should_park(Cause::Lost, true, Duration::ZERO), "grace 0 switches it off");
    }

    #[test]
    fn the_same_peer_with_the_same_options_resumes() {
        let now = Instant::now();
        let mut p = Parking::default();
        let (_, old) = p.park("1-4", "aa", 5, 42u32, now + G);
        assert!(old.is_none());
        assert!(p.contains("1-4"));
        assert_eq!(p.take("1-4", "aa", 5, now + Duration::from_secs(10)), Taken::Resumed(42));
        assert!(!p.contains("1-4"), "taking removes it");
        assert_eq!(p.take("1-4", "aa", 5, now), Taken::Nothing);
    }

    #[test]
    fn another_peer_is_turned_away_until_the_time_runs_out() {
        let now = Instant::now();
        let mut p = Parking::default();
        p.park("1-4", "aa", 5, 1u32, now + G);
        assert_eq!(p.take("1-4", "bb", 5, now + Duration::from_secs(1)), Taken::Reserved);
        assert!(p.contains("1-4"), "a refused newcomer must not disturb the reservation");
        assert_eq!(p.take("1-4", "bb", 5, now + G), Taken::Stale(1), "after the deadline anyone may open it");
    }

    #[test]
    fn different_options_or_an_expired_slot_are_released_not_resumed() {
        let now = Instant::now();
        let mut p = Parking::default();
        p.park("1-4", "aa", 5, 7u32, now + G);
        assert_eq!(p.take("1-4", "aa", 1, now), Taken::Stale(7), "read-only versus read-write must never be mixed up");
        p.park("1-4", "aa", 5, 8u32, now + G);
        assert_eq!(p.take("1-4", "aa", 5, now + G + Duration::from_secs(1)), Taken::Stale(8));
    }

    #[test]
    fn a_timer_only_expires_the_slot_it_was_started_for() {
        let now = Instant::now();
        let mut p = Parking::default();
        let (first, _) = p.park("1-4", "aa", 5, 1u32, now + G);
        let (second, displaced) = p.park("1-4", "aa", 5, 2u32, now + G);
        assert_eq!(displaced, Some(1), "parking again hands back the old item so it can be released");
        assert_eq!(p.expire("1-4", first), None, "the first timer must not release the newer slot");
        assert_eq!(p.expire("1-4", second), Some(2));
        assert_eq!(p.expire("1-4", second), None);
    }

    #[test]
    fn remove_and_drain_release_everything() {
        let now = Instant::now();
        let mut p = Parking::default();
        p.park("1-4", "aa", 0, 1u32, now + G);
        p.park("2-1", "aa", 0, 2u32, now + G);
        assert_eq!(p.remove("1-4"), Some(1));
        assert!(p.contains("2-1"));
        assert_eq!(p.drain(), vec![2]);
        assert!(!p.contains("2-1"));
    }

    #[test]
    fn the_grace_period_has_a_sane_default_and_a_cap() {
        // Not touching the environment here (tests run in parallel): just the arithmetic of the default.
        assert_eq!(DEFAULT_GRACE_SECS, 60);
        assert!(grace() <= Duration::from_secs(3600));
    }
}
