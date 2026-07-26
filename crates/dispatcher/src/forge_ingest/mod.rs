//! **forge-ingest** — the bounded context where work and code cross the
//! platform's edge: the outside forge (GitHub) on one side, operator-dispatched
//! advisory runs on the other (NORTH-STAR §1, refactor-plan C8).
//!
//! Its members share a charter: they talk to something the dispatcher does not
//! own. [`origin`] links a project to a GitHub origin and ships work as PRs off
//! pushed `chug/release-{n}` snapshots, [`github`] is the thin REST client that
//! surface calls, and [`triage`] runs an advisory agent over a stuck job and
//! writes back an assessment. Like platform-ops, no member decides a state
//! transition — triage is explicitly advisory (§1.2), and origin's release
//! surface reports rather than drives.
//!
//! This is the one dispatcher subsystem NORTH-STAR §1 names as worth
//! considering as its own process someday, since it is the only one not part of
//! the single-writer state loop's core job. Until then it is a
//! compile/visibility boundary: [`origin`] and [`triage`] run inside the actor
//! (origin git ops need the age identity for the deploy key; hold/reset/pump
//! are actor state), and credentials never leave it — origin secrets live under
//! [`origin::RESERVED_SECRET_PREFIX`] and are never injected into containers,
//! and [`github`] resolves the PAT per call rather than holding it.
//!
//! - **Accepts:** `req.projects.link`, `req.origin.*`, and operator triage
//!   requests; project secrets for the deploy key and PAT.
//! - **Emits:** origin git ops and GitHub PRs; a `TaskPhase::Triage` task
//!   carrying the written assessment.
//! - **Guarantees:** no member drives a job transition; origin credentials
//!   never enter containers and the PAT is never held.
//! - **Spec:** §1.2, §5.3.

pub mod github;
pub mod origin;
pub mod triage;
