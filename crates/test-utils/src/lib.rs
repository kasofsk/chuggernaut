//! Test harness: fake container backend / agent provider, NATS harness,
//! temp-repo builder, fixture seeding (testing.md tiers 1–2).

pub mod backend_suite;
pub mod fixture;
pub mod nats;
pub mod repo;
pub mod wait;

/// A unique, NATS-safe namespace prefix for one test (#206). Every stream, KV
/// bucket, object store, and subject a test touches carries this prefix (via
/// [`store::NatsStore::connect_namespaced`]), so many tests share one server
/// without colliding. Shape `t{8 hex}-`: a single subject token (no `.`) that is
/// also a legal KV bucket-name fragment (`[A-Za-z0-9_-]`).
pub fn unique_prefix() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("t{}-", &id[..16])
}

use agent::{AgentError, AgentOutput, AgentProvider, AgentRunConfig};
use async_trait::async_trait;
use container::{
    BackendError, ContainerBackend, ContainerId, ContainerLaunchConfig, ContainerStatus, LogTail,
    NodeStatus, RunningContainer,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Deterministic, scriptable [`ContainerBackend`]: every launch records its config
/// and "exits" with the next scripted exit code (default 0).
pub struct FakeBackend {
    next_id: AtomicU64,
    state: Mutex<FakeBackendState>,
}

/// A side-effect hook awaited when a container launches — the place to
/// simulate the outside world moving (e.g. another job landing on `main`
/// while an evaluation container runs). Consumed in launch order like
/// [`FakeProvider`]'s run hooks.
type LaunchHook =
    Box<dyn FnOnce(ContainerLaunchConfig) -> futures::future::BoxFuture<'static, ()> + Send>;

/// Decides whether a launch is rejected: `Some(err)` makes `launch` return that
/// error before any container exists — the backend refusing a container up front
/// (bad image / invalid limit → `Launch`; fleet at capacity → `NoCapacity`).
type LaunchFail = Box<dyn Fn(&ContainerLaunchConfig) -> Option<BackendError> + Send>;

#[derive(Default)]
struct FakeBackendState {
    /// Exit codes handed out in launch order; empty → exit 0.
    scripted_exits: Vec<i32>,
    launches: Vec<ContainerLaunchConfig>,
    /// Hooks consumed in launch order, awaited before `launch` returns.
    launch_hooks: Vec<Option<LaunchHook>>,
    /// When set, consulted on every `launch`; a `Some(reason)` rejects it.
    launch_fail: Option<LaunchFail>,
    exits: HashMap<ContainerId, i32>,
    /// Containers that `inspect` reports as `Running` and `wait` blocks on —
    /// a still-alive container a restarted dispatcher re-attaches to (§3.6).
    /// Consulted only when the id is not already in `exits`.
    running: std::collections::HashSet<ContainerId>,
    /// Exit codes that resolve a *running* container's `wait` without changing
    /// what `inspect` reports — a re-attached container (still `Running` at
    /// reconcile time) that later exits, so a test drives the re-attach monitor
    /// through to its harvest-at-exit (§3.6) without racing `inspect`.
    finished: HashMap<ContainerId, i32>,
    /// Files retrievable via copy_file, keyed by (container path).
    files: HashMap<String, Vec<u8>>,
    /// Returned by `logs` (and sliced by `logs_tail`) for every container.
    logs: Vec<u8>,
    /// When set, `logs_tail` sleeps this long before returning — a stand-in
    /// for a slow/wedged node, to prove one output request never blocks others.
    logs_tail_stall: Option<Duration>,
    /// When set, `logs_tail` fails with this error — an unreachable node, so
    /// the output handler must surface an error envelope rather than hang.
    logs_tail_fail: Option<String>,
    /// Containers removed via `remove`, in call order.
    removed: Vec<ContainerId>,
    /// Containers killed via `kill`, in call order (the §3.6 fleet-sweep reaps).
    killed: Vec<ContainerId>,
    /// Ids returned by `list_managed_exited` (the startup-sweep candidates).
    managed_exited: Vec<ContainerId>,
    /// Containers returned by `list_managed_running` (the fleet-sweep set).
    managed_running: Vec<RunningContainer>,
    /// When set, `list_managed_running` fails with this error — a node the
    /// sweep must tolerate (log, continue) rather than crash on.
    list_running_fail: Option<String>,
    /// Announces applied via `register_worker`, in call order (spec §3.1 dynamic
    /// registration) — the way a test asserts an announce reached the backend.
    registered: Vec<(String, u32, Option<String>)>,
    /// Workers deregistered via `mark_worker_unschedulable`, in call order.
    unschedulable: Vec<String>,
    /// When set, the fake reports `supports_dynamic_workers() == false` — a
    /// stand-in for a non-fleet backend (single-node Docker) that cannot route to
    /// announced nodes, so the dispatcher must drop stray announces.
    no_dynamic_workers: bool,
    /// What `fleet_status` reports — the live per-node health/version/refresh a
    /// real backend fills from worker pings (spec §3.1, ticket #187). Empty by
    /// default (the fake tracks capacity via `register_worker`, not health).
    fleet_status: Vec<NodeStatus>,
    /// Per-node observed capacity, applied through the same ordering rule the
    /// real fleet backend uses (spec §3.1 slot source). The real backend owns
    /// observed capacity and reports it back through `fleet_status`, so the fake
    /// has to as well — otherwise a dispatcher test would read an announced slot
    /// count that no backend ever confirmed.
    observed: std::collections::HashMap<String, (u32, types::ObservedCapacity)>,
    /// `set_slots` commands the backend received, in call order (design #293 §3/§4)
    /// — how a test asserts what the reconciler pushed, and how often.
    slot_commands: Vec<(String, u32)>,
    /// How the fake answers `set_slots`, per node. Absent → the daemon adopts the
    /// value and the observation lands (the converging case). A test scripts a
    /// refusal, a silent ignore (an old build), or a transport failure.
    slot_replies: std::collections::HashMap<String, SlotReply>,
}

/// How [`FakeBackend`] answers a `set_slots` push (design #293 §4). One variant
/// per outcome the reconciler has to distinguish.
#[derive(Debug, Clone)]
pub enum SlotReply {
    /// Adopt the value and report it back through `fleet_status`, as a daemon
    /// that accepts and re-announces does.
    Adopt,
    /// Adopt the reply but never change what the node reports — a daemon that
    /// acknowledges and reverts, or an old build that ignores the op. Must surface
    /// as `unacknowledged`, never as converged.
    AdoptWithoutObserving,
    /// Refuse the value above the node's ceiling, with the reason the UI shows.
    /// Terminal: the dispatcher must stop re-pushing it.
    Refuse { slots_max: u32, note: String },
    /// The RPC never reached the node.
    Transport(String),
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeBackend {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            state: Mutex::new(FakeBackendState::default()),
        }
    }

    /// Queue exit codes for upcoming launches (consumed in order).
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn script_exits(&self, codes: impl IntoIterator<Item = i32>) {
        self.state.lock().unwrap().scripted_exits.extend(codes);
    }

    /// Make a file available to `copy_file` (e.g. `/workspace/eval-result.json`).
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn put_file(&self, path: &str, contents: impl Into<Vec<u8>>) {
        self.state
            .lock()
            .unwrap()
            .files
            .insert(path.to_string(), contents.into());
    }

    /// Script what `logs` returns for every container (and what `logs_tail`
    /// slices its cursor pages from).
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn put_logs(&self, contents: impl Into<Vec<u8>>) {
        self.state.lock().unwrap().logs = contents.into();
    }

    /// Make `logs_tail` sleep before returning — simulate a slow/wedged node so
    /// a test can prove that one stalled output read never blocks other reads.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn stall_logs_tail(&self, delay: Duration) {
        self.state.lock().unwrap().logs_tail_stall = Some(delay);
    }

    /// Make `logs_tail` fail with `BackendError::Unavailable` — an unreachable
    /// node, so the output handler must reply with an error envelope.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn fail_logs_tail(&self, reason: impl Into<String>) {
        self.state.lock().unwrap().logs_tail_fail = Some(reason.into());
    }

    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn launches(&self) -> Vec<ContainerLaunchConfig> {
        self.state.lock().unwrap().launches.clone()
    }

    /// Queue a side-effect hook for the next un-hooked launch. The hook is
    /// awaited before `launch` returns the container id — the window during
    /// which the "container" is running (e.g. move `main` while an evaluation
    /// container runs, so the wrap-up merge gate fires).
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn on_launch<F, Fut>(&self, hook: F)
    where
        F: FnOnce(ContainerLaunchConfig) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.state
            .lock()
            .unwrap()
            .launch_hooks
            .push(Some(Box::new(move |cfg| Box::pin(hook(cfg)))));
    }

    /// Reject any launch for which `f` returns `Some(reason)` with
    /// `BackendError::Launch(reason)` — the backend refusing a container before
    /// it ever starts (bad image, invalid resource limit, node pressure). A
    /// rejected launch produces no container and consumes no scripted exit.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn fail_launch_if<F>(&self, f: F)
    where
        F: Fn(&ContainerLaunchConfig) -> Option<String> + Send + 'static,
    {
        self.state.lock().unwrap().launch_fail =
            Some(Box::new(move |c| f(c).map(BackendError::Launch)));
    }

    /// Reject launches with `BackendError::NoCapacity` — the fleet-at-capacity
    /// signal the dispatcher queues on (spec §3.5) instead of failing the task.
    /// `f` returning `Some(reason)` refuses that launch; flipping it (via shared
    /// state a test captures) frees capacity so the queued launch can proceed.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn fail_launch_no_capacity_if<F>(&self, f: F)
    where
        F: Fn(&ContainerLaunchConfig) -> Option<String> + Send + 'static,
    {
        self.state.lock().unwrap().launch_fail =
            Some(Box::new(move |c| f(c).map(BackendError::NoCapacity)));
    }

    /// Seed a container as already exited with a specific code, so `inspect`
    /// reports `Exited { exit_code }` and `wait` returns it — a container that
    /// ran and exited before this (restarted) dispatcher observed it, as restart
    /// reconciliation (§3.6) would find one.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn seed_exited(&self, id: impl Into<ContainerId>, exit_code: i32) {
        self.state
            .lock()
            .unwrap()
            .exits
            .insert(id.into(), exit_code);
    }

    /// Seed containers as still running: `inspect` reports `Running` and `wait`
    /// blocks (as a live container would). Lets a test exercise the §3.6 restart
    /// re-attach path, where reconciliation finds a Running task's container
    /// still alive and resumes monitoring it rather than failing the task.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn seed_running(&self, ids: impl IntoIterator<Item = ContainerId>) {
        self.state.lock().unwrap().running.extend(ids);
    }

    /// Exit a container previously seeded via [`seed_running`]: its `wait`
    /// resolves with `exit_code` while `inspect` kept reporting `Running` right
    /// up to the exit. Lets a test drive the §3.6 re-attach monitor through to
    /// its harvest-at-exit (the #187 deploy report) without racing the
    /// reconcile-time `inspect`.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn finish_running(&self, id: impl Into<ContainerId>, exit_code: i32) {
        self.state
            .lock()
            .unwrap()
            .finished
            .insert(id.into(), exit_code);
    }

    /// Seed the ids that `list_managed_exited` reports — the exited managed
    /// containers a startup sweep should consider.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn seed_managed_exited(&self, ids: impl IntoIterator<Item = ContainerId>) {
        self.state.lock().unwrap().managed_exited.extend(ids);
    }

    /// Seed the running managed containers `list_managed_running` reports — the
    /// §3.6 fleet-sweep set. Each carries the `(project, job, task)` identity a
    /// real container's labels would (or `None`, for a pre-labels orphan).
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn seed_managed_running(&self, containers: impl IntoIterator<Item = RunningContainer>) {
        self.state
            .lock()
            .unwrap()
            .managed_running
            .extend(containers);
    }

    /// Replace the running managed containers `list_managed_running` reports.
    /// Unlike [`seed_managed_running`] (which appends), this sets the whole set —
    /// the way to simulate a container exiting (drop it) after a launch.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn set_managed_running(&self, containers: impl IntoIterator<Item = RunningContainer>) {
        self.state.lock().unwrap().managed_running = containers.into_iter().collect();
    }

    /// Make `list_managed_running` fail — an unreachable node the fleet sweep
    /// must tolerate (log, continue) without crashing the dispatcher.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn fail_list_managed_running(&self, reason: impl Into<String>) {
        self.state.lock().unwrap().list_running_fail = Some(reason.into());
    }

    /// Container ids passed to `remove`, in call order.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn removed(&self) -> Vec<ContainerId> {
        self.state.lock().unwrap().removed.clone()
    }

    /// Container ids passed to `kill`, in call order — the §3.6 fleet-sweep
    /// reaps and any explicit gate/supersede kills.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn killed(&self) -> Vec<ContainerId> {
        self.state.lock().unwrap().killed.clone()
    }

    /// Announces the backend received via `register_worker`, in call order
    /// (spec §3.1 dynamic registration).
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn registered(&self) -> Vec<(String, u32, Option<String>)> {
        self.state.lock().unwrap().registered.clone()
    }

    /// Workers the backend was told to deregister via `mark_worker_unschedulable`.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn unschedulable(&self) -> Vec<String> {
        self.state.lock().unwrap().unschedulable.clone()
    }

    /// Model a non-fleet backend (single-node Docker): `supports_dynamic_workers`
    /// then reports `false`, so the dispatcher drops stray worker announces
    /// instead of inserting a phantom node into the roster.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn disable_dynamic_workers(&self) {
        self.state.lock().unwrap().no_dynamic_workers = true;
    }

    /// Set what `fleet_status` reports — the live per-node health/version/refresh
    /// a real backend fills from worker pings (spec §3.1, ticket #187). Lets a
    /// test prove a ping-reported refresh outcome flows into the published
    /// `FleetStatus`.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn set_fleet_status(&self, statuses: impl IntoIterator<Item = NodeStatus>) {
        self.state.lock().unwrap().fleet_status = statuses.into_iter().collect();
    }

    /// Script how a node answers a capacity push (design #293 §4): adopt, adopt
    /// without ever reporting it, refuse, or fail in transport.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn script_slot_reply(&self, node: &str, reply: SlotReply) {
        self.state
            .lock()
            .unwrap()
            .slot_replies
            .insert(node.to_string(), reply);
    }

    /// `set_slots` pushes the backend received, in call order — the way a test
    /// asserts the one-push-per-node-per-tick bound and that a refusal is terminal.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn slot_commands(&self) -> Vec<(String, u32)> {
        self.state.lock().unwrap().slot_commands.clone()
    }
}

#[async_trait]
impl ContainerBackend for FakeBackend {
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    async fn launch(&self, config: ContainerLaunchConfig) -> Result<ContainerId, BackendError> {
        if let Some(err) = self
            .state
            .lock()
            .unwrap()
            .launch_fail
            .as_ref()
            .and_then(|f| f(&config))
        {
            return Err(err);
        }
        let id = format!("fake-{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        let hook = {
            let mut st = self.state.lock().unwrap();
            let exit = if st.scripted_exits.is_empty() {
                0
            } else {
                st.scripted_exits.remove(0)
            };
            let idx = st.launches.len();
            let hook = st.launch_hooks.get_mut(idx).and_then(Option::take);
            st.launches.push(config.clone());
            st.exits.insert(id.clone(), exit);
            hook
        };
        if let Some(hook) = hook {
            hook(config).await;
        }
        Ok(id)
    }

    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    async fn wait(&self, id: &ContainerId) -> Result<i32, BackendError> {
        enum Wait {
            Exited(i32),
            Running,
            Gone,
        }
        loop {
            let outcome = {
                let st = self.state.lock().unwrap();
                if let Some(code) = st.exits.get(id).copied() {
                    Wait::Exited(code)
                } else if let Some(code) = st.finished.get(id).copied() {
                    Wait::Exited(code)
                } else if st.running.contains(id) {
                    Wait::Running
                } else {
                    Wait::Gone
                }
            };
            match outcome {
                Wait::Exited(code) => return Ok(code),
                Wait::Running => tokio::time::sleep(Duration::from_millis(5)).await,
                Wait::Gone => return Err(BackendError::NotFound(id.clone())),
            }
        }
    }

    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    async fn kill(&self, id: &ContainerId) -> Result<(), BackendError> {
        self.state.lock().unwrap().killed.push(id.clone());
        Ok(())
    }

    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    async fn inspect(&self, id: &ContainerId) -> Result<Option<ContainerStatus>, BackendError> {
        let st = self.state.lock().unwrap();
        if let Some(&exit_code) = st.exits.get(id) {
            Ok(Some(ContainerStatus::Exited { exit_code }))
        } else if st.running.contains(id) {
            Ok(Some(ContainerStatus::Running))
        } else {
            Ok(None)
        }
    }

    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    async fn copy_file(
        &self,
        _id: &ContainerId,
        path: &str,
    ) -> Result<Option<Vec<u8>>, BackendError> {
        Ok(self.state.lock().unwrap().files.get(path).cloned())
    }

    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    async fn logs(&self, _id: &ContainerId) -> Result<Vec<u8>, BackendError> {
        Ok(self.state.lock().unwrap().logs.clone())
    }

    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    async fn logs_tail(&self, _id: &ContainerId, since: u64) -> Result<LogTail, BackendError> {
        let (logs, stall, fail) = {
            let st = self.state.lock().unwrap();
            (
                st.logs.clone(),
                st.logs_tail_stall,
                st.logs_tail_fail.clone(),
            )
        };
        if let Some(delay) = stall {
            tokio::time::sleep(delay).await;
        }
        if let Some(reason) = fail {
            return Err(BackendError::Unavailable(reason));
        }
        Ok(LogTail::slice(&logs, since))
    }

    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    async fn remove(&self, id: &ContainerId) -> Result<(), BackendError> {
        let mut st = self.state.lock().unwrap();
        st.exits.remove(id);
        st.running.remove(id);
        st.finished.remove(id);
        st.managed_exited.retain(|c| c != id);
        st.removed.push(id.clone());
        Ok(())
    }

    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    async fn list_managed_exited(&self) -> Result<Vec<ContainerId>, BackendError> {
        Ok(self.state.lock().unwrap().managed_exited.clone())
    }

    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    async fn list_managed_running(&self) -> Result<Vec<RunningContainer>, BackendError> {
        let st = self.state.lock().unwrap();
        if let Some(reason) = &st.list_running_fail {
            return Err(BackendError::Unavailable(reason.clone()));
        }
        Ok(st.managed_running.clone())
    }

    /// Model a worker announce (spec §3.1 dynamic registration): record it and,
    /// as the fake's stand-in for capacity appearing, clear any `launch_fail` so
    /// a launch the fleet was refusing for NoCapacity now proceeds. Returns
    /// `true` (membership/capacity changed) so the caller re-drains the queue.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    fn register_worker(
        &self,
        name: &str,
        capacity: types::CapacityObservation,
        version: Option<String>,
    ) -> bool {
        let mut st = self.state.lock().unwrap();
        st.registered
            .push((name.to_string(), capacity.slots, version));
        let entry = st.observed.entry(name.to_string()).or_default();
        if entry.1.apply(&capacity, chrono::Utc::now()) {
            entry.0 = capacity.slots;
        }
        st.launch_fail = None;
        true
    }

    /// The fake models a dynamic worker fleet (it records registrations and
    /// clears NoCapacity), so it opts into dynamic registration by default —
    /// matching the real [`FleetBackend`] and letting the dispatcher apply its
    /// roster mutation. [`FakeBackend::disable_dynamic_workers`] flips it off to
    /// model a non-fleet backend.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    fn supports_dynamic_workers(&self) -> bool {
        !self.state.lock().unwrap().no_dynamic_workers
    }

    /// Explicitly-set health entries, with observed capacity folded in for every
    /// node that has announced — the shape a real fleet backend reports (spec
    /// §3.1 slot source). A node known only through an announce gets its own
    /// entry, available unless it has since been deregistered, so the
    /// dispatcher's snapshot sees capacity from the backend rather than from the
    /// roster's boot seed.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    fn fleet_status(&self) -> Vec<NodeStatus> {
        let st = self.state.lock().unwrap();
        let mut out = st.fleet_status.clone();
        for (name, (slots, observed)) in &st.observed {
            match out.iter_mut().find(|s| &s.name == name) {
                Some(entry) => {
                    entry.slots = Some(*slots);
                    entry.capacity = Some(*observed);
                }
                None => out.push(NodeStatus {
                    name: name.clone(),
                    available: !st.unschedulable.contains(name),
                    version: None,
                    refresh_outcome: None,
                    slots: Some(*slots),
                    capacity: Some(*observed),
                }),
            }
        }
        out
    }

    /// Model a capacity push (design #293 §3/§4): record it, then answer per the
    /// node's scripted reply — adopting also installs the observation, since a real
    /// daemon re-announces the moment it adopts.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    async fn set_node_slots(
        &self,
        node: &str,
        slots: u32,
    ) -> Result<types::worker::SetSlotsOk, BackendError> {
        let mut st = self.state.lock().unwrap();
        st.slot_commands.push((node.to_string(), slots));
        let reply = st
            .slot_replies
            .get(node)
            .cloned()
            .unwrap_or(SlotReply::Adopt);
        let adopts_observation = matches!(reply, SlotReply::Adopt);
        let (accepted, in_force, slots_max, note) = match reply {
            SlotReply::Adopt | SlotReply::AdoptWithoutObserving => (true, slots, 8, None),
            SlotReply::Refuse { slots_max, note } => {
                let held = st.observed.get(node).map_or(0, |(s, _)| *s);
                (false, held, slots_max, Some(note))
            }
            SlotReply::Transport(message) => return Err(BackendError::Unavailable(message)),
        };
        if adopts_observation {
            let entry = st.observed.entry(node.to_string()).or_default();
            let observation = types::CapacityObservation {
                slots,
                slots_max: Some(slots_max),
                mark: (entry.1.mark.0.max(1), entry.1.mark.1 + 1),
                transport: types::CapacityTransport::Announce,
            };
            if entry.1.apply(&observation, chrono::Utc::now()) {
                entry.0 = slots;
            }
        }
        Ok(types::worker::SetSlotsOk {
            accepted,
            slots: in_force,
            slots_max,
            capacity_epoch: 1,
            capacity_generation: 1,
            note,
        })
    }

    /// Model heartbeat loss (spec §3.1): record the deregistration and, as the
    /// fake's stand-in for the announced capacity vanishing, refuse new launches
    /// with NoCapacity again — so a test sees new placements queue while any
    /// already-running container (seeded in `managed_running`) stays tracked.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    fn mark_worker_unschedulable(&self, name: &str) {
        let mut st = self.state.lock().unwrap();
        st.unschedulable.push(name.to_string());
        st.launch_fail = Some(Box::new(|_| {
            Some(BackendError::NoCapacity("worker heartbeat lost".into()))
        }));
    }
}

/// Deterministic, scriptable [`AgentProvider`]: every run records its config
/// and returns the next scripted exit code (default 0). Side effects a real
/// agent would have (commits, submit_eval calls) are scripted per run via
/// [`FakeProvider::on_run`].
pub struct FakeProvider {
    state: Mutex<FakeProviderState>,
    push_notifications: bool,
    /// When set, runs launch a real (fake) container so the run reports a
    /// container id — the handle the dispatcher needs to harvest transcripts
    /// and logs. Mirrors ClaudeProvider, which launches via the backend.
    backend: Option<Arc<dyn ContainerBackend>>,
}

/// Async so a hook can drive the dispatcher (e.g. submit_eval) and await the
/// ack before the "container" exits — exactly the ordering the channel MCP
/// server guarantees (spec §4.2 bounded-retry-until-ack).
type RunHook = Box<dyn FnOnce(AgentRunConfig) -> futures::future::BoxFuture<'static, ()> + Send>;

#[derive(Default)]
struct FakeProviderState {
    /// Exit codes handed out in run order; empty → exit 0.
    scripted_exits: Vec<i32>,
    /// Hooks consumed in run order, invoked with the run's config before it
    /// "exits" — the place to simulate submit_result/submit_eval side effects.
    hooks: Vec<Option<RunHook>>,
    runs: Vec<AgentRunConfig>,
}

impl Default for FakeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeProvider {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(FakeProviderState::default()),
            push_notifications: true,
            backend: None,
        }
    }

    /// Launch through `backend`, so runs report a container id and artifact
    /// capture can be exercised. Without this a run has no container, like a
    /// provider stub.
    pub fn with_backend(backend: Arc<dyn ContainerBackend>) -> Self {
        Self {
            backend: Some(backend),
            ..Self::new()
        }
    }

    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn script_exits(&self, codes: impl IntoIterator<Item = i32>) {
        self.state.lock().unwrap().scripted_exits.extend(codes);
    }

    /// Queue a side-effect hook for the next un-hooked run. The hook is awaited
    /// before the run returns its exit code.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn on_run<F, Fut>(&self, hook: F)
    where
        F: FnOnce(AgentRunConfig) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.state
            .lock()
            .unwrap()
            .hooks
            .push(Some(Box::new(move |cfg| Box::pin(hook(cfg)))));
    }

    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    pub fn runs(&self) -> Vec<AgentRunConfig> {
        self.state.lock().unwrap().runs.clone()
    }
}

#[async_trait]
impl AgentProvider for FakeProvider {
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    async fn run(
        &self,
        config: AgentRunConfig,
        on_launch: agent::LaunchReporter,
    ) -> Result<AgentOutput, AgentError> {
        let container_id = match &self.backend {
            Some(backend) => Some(
                backend
                    .launch(ContainerLaunchConfig {
                        image: config.image.clone(),
                        cmd: vec!["agent".into()],
                        env: config.env.clone(),
                        files: config.files.clone(),
                        cpu_limit: None,
                        memory_limit: None,
                        node: config.node.clone(),
                    })
                    .await?,
            ),
            None => None,
        };
        let (exit_code, hook) = {
            let mut st = self.state.lock().unwrap();
            let exit = if st.scripted_exits.is_empty() {
                0
            } else {
                st.scripted_exits.remove(0)
            };
            let idx = st.runs.len();
            let hook = st.hooks.get_mut(idx).and_then(Option::take);
            st.runs.push(config.clone());
            (exit, hook)
        };
        if let Some(id) = &container_id {
            on_launch.report(id);
        }
        if let Some(hook) = hook {
            hook(config.clone()).await;
        }
        Ok(AgentOutput {
            exit_code,
            container_id,
            session_id: Some(config.session_id),
        })
    }

    fn supports_push_notifications(&self) -> bool {
        self.push_notifications
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::collections::HashMap;

    fn cfg() -> ContainerLaunchConfig {
        ContainerLaunchConfig {
            image: "test:latest".into(),
            cmd: vec!["true".into()],
            env: HashMap::new(),
            files: vec![],
            cpu_limit: None,
            memory_limit: None,
            node: None,
        }
    }

    #[tokio::test]
    async fn fake_backend_scripts_exits_in_order() {
        let be = FakeBackend::new();
        be.script_exits([1, 0]);
        let a = be.launch(cfg()).await.unwrap();
        let b = be.launch(cfg()).await.unwrap();
        let c = be.launch(cfg()).await.unwrap();
        assert_eq!(be.wait(&a).await.unwrap(), 1);
        assert_eq!(be.wait(&b).await.unwrap(), 0);
        assert_eq!(be.wait(&c).await.unwrap(), 0);
        assert_eq!(be.launches().len(), 3);
    }

    #[tokio::test]
    async fn fake_backend_serves_files() {
        let be = FakeBackend::new();
        be.put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());
        let id = be.launch(cfg()).await.unwrap();
        let f = be
            .copy_file(&id, "/workspace/eval-result.json")
            .await
            .unwrap();
        assert_eq!(f.unwrap(), br#"{"ok":true}"#);
        assert!(be.copy_file(&id, "/missing").await.unwrap().is_none());
    }
}
