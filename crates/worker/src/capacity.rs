//! The worker daemon's live capacity cell (spec §3.1 slot source): the one
//! number the node reports over both transports, its ceiling, and the ordering
//! key that sequences observations dispatcher-side.
//!
//! - **Accepts:** a `set_slots` command's requested slot count
//!   ([`types::worker::SetSlotsRequest`], relayed by the dispatcher on behalf of
//!   an operator), and reads from the daemon's announce and ping paths.
//! - **Emits:** [`Capacity::set_slots`] verdicts ([`SetSlotsOk`]) and
//!   [`CapacityReport`] snapshots for the two report transports.
//! - **Guarantees:** `slots <= slots_max` always — including the first-boot
//!   value; `capacity_epoch` is constant for the life of the process and
//!   `capacity_generation` strictly increases within it, so the pair only ever
//!   moves forward; one lock spans the whole read-decide-bump, so no reader and
//!   no concurrent adoption ever sees a slot count beside another adoption's
//!   generation; a rejection changes nothing at all. Synchronous and I/O-free:
//!   the daemon owns publishing what this decides.
//! - **Spec:** §3.1 (dynamic worker registration, operator capacity control).

use types::worker::SetSlotsOk;

/// One reading of the node's capacity, for whichever transport is reporting.
/// Taken as a snapshot so an announce and a ping built from the same reading
/// cannot disagree with each other mid-adoption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityReport {
    pub slots: u32,
    pub slots_max: u32,
    pub epoch_ms: u64,
    pub generation: u64,
}

/// The node's capacity: current slots, the ceiling they are validated against,
/// and the `(epoch, generation)` ordering key. Interior-mutable so the daemon's
/// concurrent op handlers and its announce loop share one owner of the number.
#[derive(Debug)]
pub struct Capacity {
    /// The slot count and its generation move together: the pair is one
    /// ordering key, so neither a reader nor a second adoption may see a count
    /// from one adoption beside the generation of another. Two independent
    /// atomics cannot express that — the whole read-decide-bump has to be one
    /// critical section. The guard is never held across an `.await`; the daemon
    /// publishes outside it.
    cell: std::sync::Mutex<(u32, u64)>,
    /// Immutable for the life of the process: the ceiling is config, and the
    /// epoch identifies the process.
    slots_max: u32,
    epoch_ms: u64,
}

impl Capacity {
    /// Poisoning cannot corrupt the pair — nothing between the lock and the
    /// unlock can panic — so a poisoned lock is recovered rather than
    /// propagated, and the daemon keeps serving its capacity ops.
    fn cell(&self) -> std::sync::MutexGuard<'_, (u32, u64)> {
        self.cell
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `boot_slots` is the node's first-boot value (`WORKER_SLOTS`), **clamped**
    /// to `slots_max`: the ceiling states what the node can actually serve, so a
    /// boot value above it would advertise capacity the same daemon would then
    /// refuse to re-adopt — and would break the `slots <= slots_max` invariant
    /// every report carries. The caller logs the clamp; it is a misconfigured
    /// node, not a normal one.
    pub fn new(boot_slots: u32, slots_max: u32, epoch_ms: u64) -> Self {
        assert!(
            slots_max >= 1,
            "slots_max must be at least 1 — a zero ceiling could never be raised again \
             (config rejects WORKER_SLOTS_MAX=0)"
        );
        let capacity = Self {
            cell: std::sync::Mutex::new((boot_slots.min(slots_max), 0)),
            slots_max,
            epoch_ms,
        };
        debug_assert!(capacity.report().slots <= slots_max, "boot value clamped");
        capacity
    }

    /// Snapshot for a report transport (announce or ping reply). One lock, so
    /// the count and the generation always come from the same adoption.
    pub fn report(&self) -> CapacityReport {
        let (slots, generation) = *self.cell();
        CapacityReport {
            slots,
            slots_max: self.slots_max,
            epoch_ms: self.epoch_ms,
            generation,
        }
    }

    /// Decide an operator's `set_slots` command: adopt anything the node can
    /// serve, refuse anything above the ceiling.
    ///
    /// Below the ceiling the operator is trusted — no memory or disk heuristics
    /// (design #293 §6) — and `0` is a legitimate full drain. An adoption bumps
    /// the generation even when the number is unchanged, so a re-push is still
    /// ordered ahead of whatever the dispatcher last applied; a rejection bumps
    /// nothing, which is what makes it terminal rather than a value that
    /// oscillates.
    pub fn set_slots(&self, requested: u32) -> SetSlotsOk {
        // Read, decide, and bump under one guard: the reply names the pair this
        // call installed, which is what lets the dispatcher order two racing
        // adoptions by generation and land on the number the node is running.
        let mut cell = self.cell();
        let (slots_before, generation_before) = *cell;
        if requested > self.slots_max {
            return SetSlotsOk {
                accepted: false,
                slots: slots_before,
                slots_max: self.slots_max,
                capacity_epoch: self.epoch_ms,
                capacity_generation: generation_before,
                note: Some(format!(
                    "requested {requested} slots exceeds this node's maximum of {} \
                     (WORKER_SLOTS_MAX, default the node's CPU count)",
                    self.slots_max
                )),
            };
        }
        *cell = (requested, generation_before + 1);
        let (slots, generation) = *cell;
        debug_assert!(slots <= self.slots_max, "adopted above the ceiling");
        debug_assert!(
            generation > generation_before,
            "an adoption must move the ordering key forward"
        );
        let reply = SetSlotsOk {
            accepted: true,
            slots,
            slots_max: self.slots_max,
            capacity_epoch: self.epoch_ms,
            capacity_generation: generation,
            note: None,
        };
        debug_assert!(
            (reply.slots, reply.capacity_generation) == *cell,
            "the reply must name the pair the cell now holds"
        );
        reply
    }
}

/// The capacity epoch: wall-clock unix **milliseconds**, stamped once at daemon
/// start from the node's own clock.
///
/// Milliseconds rather than the seconds design #293 §1 specified: a
/// crash-looping daemon under `--restart=always` restarts twice inside one
/// second, and an equal epoch with the generation reset to 0 has its announces
/// discarded against the dispatcher's watermark. The ping backstop recovers
/// that, but `probe_worker` runs only at startup and on the placement path, so
/// on an idle fleet the displayed number stays stale until the next launch.
/// A pre-1970 clock reads as 0, which the ping backstop resets like any other
/// epoch anomaly.
pub fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::collections::HashMap;

    /// The boot value never exceeds the ceiling: a node told to start at 8 on a
    /// 4-slot ceiling comes up at 4, so it never advertises capacity its own
    /// `set_slots` would refuse.
    #[test]
    fn boot_value_is_clamped_to_the_ceiling() {
        let capacity = Capacity::new(8, 4, 1_769_000_000_123);
        let report = capacity.report();
        assert_eq!(report.slots, 4);
        assert_eq!(report.slots_max, 4);
        assert_eq!(report.epoch_ms, 1_769_000_000_123);
        assert_eq!(report.generation, 0, "no adoption has happened yet");

        // Under the ceiling the boot value stands verbatim.
        assert_eq!(Capacity::new(2, 6, 1).report().slots, 2);
    }

    /// Adoption bumps the generation and leaves the epoch alone — the pair is
    /// the ordering key, and only its second half moves within one process.
    #[test]
    fn adoption_bumps_the_generation() {
        let capacity = Capacity::new(4, 6, 1_769_000_000_123);

        let ok = capacity.set_slots(2);
        assert!(ok.accepted);
        assert_eq!(ok.slots, 2);
        assert_eq!(ok.slots_max, 6);
        assert_eq!(ok.capacity_generation, 1);
        assert_eq!(ok.capacity_epoch, 1_769_000_000_123);
        assert_eq!(ok.note, None);
        assert_eq!(
            capacity.report().slots,
            2,
            "the report agrees with the reply"
        );

        // A full drain is an ordinary adoption, and the boundary value fits.
        let drained = capacity.set_slots(0);
        assert!(drained.accepted);
        assert_eq!((drained.slots, drained.capacity_generation), (0, 2));
        let at_max = capacity.set_slots(6);
        assert!(at_max.accepted, "the ceiling itself is adoptable");
        assert_eq!(at_max.capacity_generation, 3);

        // Re-adopting the same number still moves the key forward, so a
        // dispatcher re-push is never ordered behind what it is re-asserting.
        let again = capacity.set_slots(6);
        assert_eq!((again.slots, again.capacity_generation), (6, 4));
        assert_eq!(
            again.capacity_epoch, 1_769_000_000_123,
            "epoch is stamped once"
        );
    }

    /// Above the ceiling is a rejection carrying a reason, and it changes
    /// nothing: not the slot count, not the generation. That is what lets the
    /// dispatcher treat a rejection as terminal.
    #[test]
    fn rejection_above_the_ceiling_changes_nothing() {
        let capacity = Capacity::new(4, 6, 1_769_000_000_123);
        assert!(capacity.set_slots(2).accepted);

        let rejected = capacity.set_slots(7);
        assert!(!rejected.accepted);
        assert_eq!(rejected.slots, 2, "the adopted value stays in force");
        assert_eq!(rejected.slots_max, 6);
        assert_eq!(rejected.capacity_generation, 1, "no adoption, no bump");
        assert!(
            rejected
                .note
                .as_deref()
                .is_some_and(|note| note.contains('6') && note.contains('7')),
            "the reason must name both numbers for the UI: {rejected:?}"
        );
        assert_eq!(capacity.report().slots, 2);
    }

    /// The pair is one ordering key, so no observer may ever see a slot count
    /// beside another adoption's generation. The daemon really does handle
    /// requests concurrently (`daemon::run` spawns each into a `JoinSet`), and
    /// the dispatcher applies only observations at-or-above its watermark — so a
    /// crossed pair pins the wrong number until the next ping, which on an idle
    /// fleet is the next launch. Repeated because the window is narrow.
    #[test]
    fn concurrent_adoptions_never_cross_the_pair() {
        for _round in 0..50 {
            concurrent_adoptions_round(8);
        }
    }

    /// The node's boot value for the race below — the count reported under
    /// generation 0, before any adoption.
    const RACE_BOOT_SLOTS: u32 = 1;

    /// One round: `racers` adoptions of distinct counts all released at once,
    /// while a reader spins on `report()` throughout. Asserts that generations
    /// are distinct, that every count a reader saw is the one installed under
    /// the generation it saw it with, and that the highest generation names the
    /// count left in force.
    fn concurrent_adoptions_round(racers: u32) {
        use std::sync::atomic::{AtomicBool, Ordering};
        let capacity =
            std::sync::Arc::new(Capacity::new(RACE_BOOT_SLOTS, racers, 1_769_000_000_123));
        let start = std::sync::Arc::new(std::sync::Barrier::new(racers as usize + 1));
        let racing = std::sync::Arc::new(AtomicBool::new(true));
        let reader = {
            let (capacity, start, racing) = (capacity.clone(), start.clone(), racing.clone());
            std::thread::spawn(move || {
                start.wait();
                let mut seen = Vec::new();
                while racing.load(Ordering::Relaxed) {
                    seen.push(capacity.report());
                }
                seen
            })
        };
        let adopters: Vec<_> = (1..=racers)
            .map(|requested| {
                let (capacity, start) = (capacity.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    capacity.set_slots(requested)
                })
            })
            .collect();
        let mut replies: Vec<SetSlotsOk> = adopters
            .into_iter()
            .map(|adopter| adopter.join().expect("adoption panicked"))
            .collect();
        racing.store(false, Ordering::Relaxed);
        let seen = reader.join().expect("reader panicked");
        replies.sort_by_key(|reply| reply.capacity_generation);

        assert_eq!(
            replies
                .iter()
                .map(|reply| reply.capacity_generation)
                .collect::<Vec<u64>>(),
            (1..=u64::from(racers)).collect::<Vec<u64>>(),
            "every adoption gets its own generation, none shared or skipped"
        );
        let installed: HashMap<u64, u32> = replies
            .iter()
            .map(|reply| (reply.capacity_generation, reply.slots))
            .collect();
        for report in &seen {
            let expected = installed
                .get(&report.generation)
                .copied()
                .unwrap_or(RACE_BOOT_SLOTS);
            assert_eq!(report.slots, expected, "crossed pair observed: {report:?}");
        }
        let winner = replies.last().expect("at least one racer");
        assert_eq!(
            (capacity.report().slots, capacity.report().generation),
            (winner.slots, winner.capacity_generation),
            "the highest-generation reply must name the count now in force"
        );
    }

    /// The epoch is a real millisecond wall clock — nonzero, and finer than the
    /// seconds the design specified (the two-restarts-in-one-second case).
    #[test]
    fn epoch_is_unix_milliseconds() {
        let epoch = now_epoch_ms();
        // 2020-01-01 in ms; a seconds-precision stamp would be ~1000x smaller.
        assert!(epoch > 1_577_836_800_000, "not milliseconds: {epoch}");
    }
}
