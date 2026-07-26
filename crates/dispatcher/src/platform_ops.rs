//! Dispatcher-side adapter for the **platform-ops** context, which lives in its
//! own crate (`chuggernaut-platform-ops`, refactor-plan C9).
//!
//! The context's charter — live platform visibility plus post-run housekeeping,
//! never a job transition — is stated on the crate root; what is left here is
//! the seam. The context takes no `&mut Core` (that is the condition it had to
//! meet to graduate to a crate), so this module is what turns the single
//! writer's fields into the narrow views it does take:
//! [`fleet::JobLookup`] over the in-memory graphs, a [`fleet::FleetView`], and
//! the borrowed [`cd::ConfigSnapshot`]. Both entry points are called on the
//! actor thread, so the snapshots they publish never race a state write; the
//! submodules are re-exported so `crate::platform_ops::…` call sites read the
//! same as they did when the context was a directory here.
//!
//! - **Accepts:** `&mut Core` at two call sites — the occupancy-relevant
//!   message (`core`) and the scan tick (`scan`).
//! - **Emits:** the context's views, and the republish state back into `Core`.
//! - **Guarantees:** no decision of its own — every line is field gathering;
//!   nothing here writes a job or task record.
//! - **Spec:** §3.1, §3.6; CD plan C.

pub use chuggernaut_platform_ops::{cd, fleet, harvest, seed};

use crate::core::Core;

impl fleet::JobLookup for Core {
    /// Off the in-memory graphs, never a KV read: occupancy is republished on
    /// every occupancy-relevant message, so this runs hot and must not do I/O.
    fn identify(&self, project: &str, seq: u64) -> Option<fleet::JobIdentity> {
        let job = self.graphs.get(project).and_then(|g| g.get(seq))?;
        Some(fleet::JobIdentity {
            job_type: job.r#type.clone(),
            state: job.state,
        })
    }
}

impl Core {
    /// Recompute live fleet occupancy and republish it when it moved (spec
    /// §3.1). Best-effort inside the context; this half only lends it the view.
    pub(crate) async fn refresh_fleet_status(&mut self) {
        let view = fleet::FleetView {
            backend: self.backend.as_ref(),
            tasks: &self.tasks,
            roster: &self.fleet_roster,
            queue_depth: self.launch_queue.len() as u32,
            jobs: self,
        };
        let status = fleet::compute(&view).await;
        fleet::publish(&status, &self.store, &mut self.last_fleet_status).await;
    }

    /// Keep the platform config snapshot fresh off the scan tick (CD plan C).
    /// The republish state is taken and put back so the context can borrow it
    /// mutably while the ports it needs are borrowed from `Core` immutably.
    pub(crate) async fn refresh_config_snapshot(&mut self) {
        let Some(mut snap) = self.snapshot.take() else {
            return;
        };
        cd::refresh(
            &mut snap,
            &self.store,
            &self.repos,
            &self.backend.fleet_status(),
            &self.fleet_roster,
        )
        .await;
        self.snapshot = Some(snap);
    }
}
