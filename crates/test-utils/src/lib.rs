//! Test harness: fake container backend / agent provider, NATS harness,
//! temp-repo builder, fixture seeding (testing.md tiers 1–2).

pub mod backend_suite;
pub mod nats;
pub mod repo;
pub mod wait;

/// A unique, NATS-safe namespace prefix for one test (#206). Every stream, KV
/// bucket, object store, and subject a test touches carries this prefix (via
/// [`store::NatsStore::connect_namespaced`]), so many tests share one server
/// without colliding. Shape `t{8 hex}-`: a single subject token (no `.`) that is
/// also a legal KV bucket-name fragment (`[A-Za-z0-9_-]`).
pub fn unique_prefix() -> String {
    // 16 hex chars (~64 bits): under the communal one-server gate every
    // per-test prefix across all binaries shares one NATS, and 32 bits of
    // uniqueness invites birthday collisions at scale (#207 review).
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
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn script_exits(&self, codes: impl IntoIterator<Item = i32>) {
        self.state.lock().unwrap().scripted_exits.extend(codes);
    }

    /// Make a file available to `copy_file` (e.g. `/workspace/eval-result.json`).
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn put_file(&self, path: &str, contents: impl Into<Vec<u8>>) {
        self.state
            .lock()
            .unwrap()
            .files
            .insert(path.to_string(), contents.into());
    }

    /// Script what `logs` returns for every container (and what `logs_tail`
    /// slices its cursor pages from).
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn put_logs(&self, contents: impl Into<Vec<u8>>) {
        self.state.lock().unwrap().logs = contents.into();
    }

    /// Make `logs_tail` sleep before returning — simulate a slow/wedged node so
    /// a test can prove that one stalled output read never blocks other reads.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn stall_logs_tail(&self, delay: Duration) {
        self.state.lock().unwrap().logs_tail_stall = Some(delay);
    }

    /// Make `logs_tail` fail with `BackendError::Unavailable` — an unreachable
    /// node, so the output handler must reply with an error envelope.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn fail_logs_tail(&self, reason: impl Into<String>) {
        self.state.lock().unwrap().logs_tail_fail = Some(reason.into());
    }

    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn launches(&self) -> Vec<ContainerLaunchConfig> {
        self.state.lock().unwrap().launches.clone()
    }

    /// Queue a side-effect hook for the next un-hooked launch. The hook is
    /// awaited before `launch` returns the container id — the window during
    /// which the "container" is running (e.g. move `main` while an evaluation
    /// container runs, so the wrap-up merge gate fires).
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
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
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
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
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
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
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
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
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn seed_running(&self, ids: impl IntoIterator<Item = ContainerId>) {
        self.state.lock().unwrap().running.extend(ids);
    }

    /// Exit a container previously seeded via [`seed_running`]: its `wait`
    /// resolves with `exit_code` while `inspect` kept reporting `Running` right
    /// up to the exit. Lets a test drive the §3.6 re-attach monitor through to
    /// its harvest-at-exit (the #187 deploy report) without racing the
    /// reconcile-time `inspect`.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn finish_running(&self, id: impl Into<ContainerId>, exit_code: i32) {
        self.state
            .lock()
            .unwrap()
            .finished
            .insert(id.into(), exit_code);
    }

    /// Seed the ids that `list_managed_exited` reports — the exited managed
    /// containers a startup sweep should consider.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn seed_managed_exited(&self, ids: impl IntoIterator<Item = ContainerId>) {
        self.state.lock().unwrap().managed_exited.extend(ids);
    }

    /// Seed the running managed containers `list_managed_running` reports — the
    /// §3.6 fleet-sweep set. Each carries the `(project, job, task)` identity a
    /// real container's labels would (or `None`, for a pre-labels orphan).
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
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
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn set_managed_running(&self, containers: impl IntoIterator<Item = RunningContainer>) {
        self.state.lock().unwrap().managed_running = containers.into_iter().collect();
    }

    /// Make `list_managed_running` fail — an unreachable node the fleet sweep
    /// must tolerate (log, continue) without crashing the dispatcher.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn fail_list_managed_running(&self, reason: impl Into<String>) {
        self.state.lock().unwrap().list_running_fail = Some(reason.into());
    }

    /// Container ids passed to `remove`, in call order.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn removed(&self) -> Vec<ContainerId> {
        self.state.lock().unwrap().removed.clone()
    }

    /// Container ids passed to `kill`, in call order — the §3.6 fleet-sweep
    /// reaps and any explicit gate/supersede kills.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn killed(&self) -> Vec<ContainerId> {
        self.state.lock().unwrap().killed.clone()
    }

    /// Announces the backend received via `register_worker`, in call order
    /// (spec §3.1 dynamic registration).
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn registered(&self) -> Vec<(String, u32, Option<String>)> {
        self.state.lock().unwrap().registered.clone()
    }

    /// Workers the backend was told to deregister via `mark_worker_unschedulable`.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn unschedulable(&self) -> Vec<String> {
        self.state.lock().unwrap().unschedulable.clone()
    }

    /// Model a non-fleet backend (single-node Docker): `supports_dynamic_workers`
    /// then reports `false`, so the dispatcher drops stray worker announces
    /// instead of inserting a phantom node into the roster.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn disable_dynamic_workers(&self) {
        self.state.lock().unwrap().no_dynamic_workers = true;
    }

    /// Set what `fleet_status` reports — the live per-node health/version/refresh
    /// a real backend fills from worker pings (spec §3.1, ticket #187). Lets a
    /// test prove a ping-reported refresh outcome flows into the published
    /// `FleetStatus`.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn set_fleet_status(&self, statuses: impl IntoIterator<Item = NodeStatus>) {
        self.state.lock().unwrap().fleet_status = statuses.into_iter().collect();
    }
}

#[async_trait]
impl ContainerBackend for FakeBackend {
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    async fn launch(&self, config: ContainerLaunchConfig) -> Result<ContainerId, BackendError> {
        // Rejected before allocating an id or recording the launch: a refused
        // container never exists, exactly as a real backend refusal.
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
        // Awaited outside the lock so the hook can drive the dispatcher, as a
        // real container's lifetime would overlap other work.
        if let Some(hook) = hook {
            hook(config).await;
        }
        Ok(id)
    }

    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
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
                    // A re-attached running container that has now exited.
                    Wait::Exited(code)
                } else if st.running.contains(id) {
                    Wait::Running
                } else {
                    Wait::Gone
                }
            };
            match outcome {
                Wait::Exited(code) => return Ok(code),
                // A still-running container never exits on its own — a
                // re-attached monitor parks here, keeping the task Running
                // (spec §3.6), until a test calls `finish_running`.
                Wait::Running => tokio::time::sleep(Duration::from_millis(5)).await,
                Wait::Gone => return Err(BackendError::NotFound(id.clone())),
            }
        }
    }

    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    async fn kill(&self, id: &ContainerId) -> Result<(), BackendError> {
        self.state.lock().unwrap().killed.push(id.clone());
        Ok(())
    }

    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
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

    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    async fn copy_file(
        &self,
        _id: &ContainerId,
        path: &str,
    ) -> Result<Option<Vec<u8>>, BackendError> {
        Ok(self.state.lock().unwrap().files.get(path).cloned())
    }

    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    async fn logs(&self, _id: &ContainerId) -> Result<Vec<u8>, BackendError> {
        Ok(self.state.lock().unwrap().logs.clone())
    }

    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
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

    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    async fn remove(&self, id: &ContainerId) -> Result<(), BackendError> {
        let mut st = self.state.lock().unwrap();
        // Drop it from the launch bookkeeping so `inspect`/`wait` afterward
        // report it as gone, mirroring a real removed container. Idempotent.
        st.exits.remove(id);
        st.running.remove(id);
        st.finished.remove(id);
        st.managed_exited.retain(|c| c != id);
        st.removed.push(id.clone());
        Ok(())
    }

    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    async fn list_managed_exited(&self) -> Result<Vec<ContainerId>, BackendError> {
        Ok(self.state.lock().unwrap().managed_exited.clone())
    }

    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
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
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    fn register_worker(&self, name: &str, slots: u32, version: Option<String>) -> bool {
        let mut st = self.state.lock().unwrap();
        st.registered.push((name.to_string(), slots, version));
        st.launch_fail = None;
        true
    }

    /// The fake models a dynamic worker fleet (it records registrations and
    /// clears NoCapacity), so it opts into dynamic registration by default —
    /// matching the real [`FleetBackend`] and letting the dispatcher apply its
    /// roster mutation. [`FakeBackend::disable_dynamic_workers`] flips it off to
    /// model a non-fleet backend.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    fn supports_dynamic_workers(&self) -> bool {
        !self.state.lock().unwrap().no_dynamic_workers
    }

    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    fn fleet_status(&self) -> Vec<NodeStatus> {
        self.state.lock().unwrap().fleet_status.clone()
    }

    /// Model heartbeat loss (spec §3.1): record the deregistration and, as the
    /// fake's stand-in for the announced capacity vanishing, refuse new launches
    /// with NoCapacity again — so a test sees new placements queue while any
    /// already-running container (seeded in `managed_running`) stays tracked.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
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

    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn script_exits(&self, codes: impl IntoIterator<Item = i32>) {
        self.state.lock().unwrap().scripted_exits.extend(codes);
    }

    /// Queue a side-effect hook for the next un-hooked run. The hook is awaited
    /// before the run returns its exit code.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
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

    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    pub fn runs(&self) -> Vec<AgentRunConfig> {
        self.state.lock().unwrap().runs.clone()
    }
}

#[async_trait]
impl AgentProvider for FakeProvider {
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::unwrap_used)]
    async fn run(
        &self,
        config: AgentRunConfig,
        on_launch: agent::LaunchReporter,
    ) -> Result<AgentOutput, AgentError> {
        // Launch first, before recording the run or consuming a hook/exit, so a
        // refused container (e.g. `NoCapacity`) short-circuits with no run
        // side-effects — mirroring a real provider whose `?` on launch precedes
        // any container work, and the dispatcher's #140 queue-on-capacity path.
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
        // Report the id the instant the "container" launches, mirroring a real
        // provider — so the dispatcher stamps it onto the Running task record.
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
        assert_eq!(be.wait(&c).await.unwrap(), 0); // default
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
