//! `chuggernaut admin ...` — user and project management (spec §12.2–12.3).
//! Ingest tokens and secret key rotation land with their consumers.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::path::PathBuf;
use store::{NatsStore, keys};
use types::User;

#[derive(clap::Args, Debug)]
pub struct AdminArgs {
    #[arg(long, global = true, default_value = "nats://localhost:4222")]
    pub nats_url: String,
    /// Where `chuggernaut init` wrote the keypairs (§12.1). Used for
    /// `dispatcher.creds` (operator-mode NATS) and `age_public.key`
    /// (secret encryption); a plain dev server needs neither.
    #[arg(long, global = true, default_value = "/data/keys")]
    pub keys_dir: PathBuf,
    #[command(subcommand)]
    pub cmd: AdminCmd,
}

#[derive(clap::Subcommand, Debug)]
pub enum AdminCmd {
    #[command(subcommand)]
    User(UserCmd),
    #[command(subcommand)]
    Project(ProjectCmd),
    #[command(subcommand)]
    Secret(SecretCmd),
    #[command(subcommand)]
    Var(VarCmd),
    #[command(subcommand)]
    CloudIdentity(CloudIdentityCmd),
    /// Mint NATS credentials for a `chuggernaut worker` node daemon
    /// (spec §3.1): allowed only its own `req.worker.{node}.>` subjects.
    /// Local-only — reads the account seed, no NATS connection.
    WorkerCreds {
        /// Node name — must match its DOCKER_NODES entry (`{node}|worker|N`)
        /// and be subject-safe ([A-Za-z0-9_-]+).
        #[arg(long)]
        node: String,
        /// Output path; defaults to `worker-{node}.creds` in the keys dir.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Mint a worker node's read-only git credential (spec §3.1 self-refresh):
    /// generate a keypair, sign a long-lived READ-ONLY cert scoped to the
    /// platform repo, and write `worker_git` + `worker_git-cert.pub` for the
    /// operator to install into the node's credential directory
    /// (deploy/prod/README.md §6). Local-only — reads the `ssh_ca` key, no NATS.
    WorkerGitKey {
        /// Node name (its DOCKER_NODES entry, `{node}|worker|N`).
        #[arg(long)]
        node: String,
        /// Platform repo the node may pull, `{owner}/{project}` — the repo the
        /// node fetches its build context from over the ssh front.
        #[arg(long)]
        project: String,
        /// Directory to write `worker_git` + `worker_git-cert.pub` into;
        /// defaults to the keys dir.
        #[arg(long)]
        out_dir: Option<PathBuf>,
        /// Cert validity in days (renew by re-running). Long-lived by default —
        /// a node credential is operator-installed, not per-job ephemeral.
        #[arg(long, default_value = "3650")]
        days: i64,
    },
    /// Ask a worker node to rebuild its images at a SHA and swap its daemon
    /// (spec §3.1 self-refresh). The dispatcher host cannot ssh a tagged worker,
    /// so the deploy inverts control and requests refresh over the worker RPC.
    /// Never fails the deploy: an unreachable/failed node is a WARNING with its
    /// version drift surfaced, not an error.
    WorkerRefresh {
        /// Node name (its `{node}|worker|N` DOCKER_NODES entry).
        #[arg(long)]
        node: String,
        /// Git SHA to build the node images at.
        #[arg(long)]
        sha: String,
        /// Image tag to build/run.
        #[arg(long, default_value = "prod")]
        tag: String,
        /// Seconds to wait for the node to come back on the new version. 0 →
        /// request and report the accept without confirming the swap (builds
        /// can take minutes; the new version still flows to the fleet snapshot).
        #[arg(long, default_value = "0")]
        wait_secs: u64,
        /// CANCEL the in-flight refresh to `--sha` on this node instead of
        /// requesting one (ticket #254). The deploy fans refreshes out to every
        /// node at once, so when one node fails it cancels the rest rather than
        /// letting them build for another ten minutes against a deploy that is
        /// already failing. A node that already swapped stays swapped — the
        /// reply says so.
        #[arg(long)]
        cancel: bool,
    },
    /// One-shot backfill of `task_time_ms` onto existing job records (spec
    /// §1.1). The dispatcher recomputes a job's task time whenever one of its
    /// tasks is written back, so only records that finished *before* the field
    /// existed need this — they would otherwise show a completion stamp with no
    /// duration forever. Same summing rule (`types::task_time_ms`), same
    /// per-job bounded read, and idempotent: re-running it changes nothing.
    ///
    /// Terminal jobs only. The dispatcher is the single writer of job records
    /// and never writes a Done/Revoked one again, so this cannot race it; a
    /// live job gets its total from the dispatcher on its next task write.
    BackfillTaskTime {
        /// `{owner}/{project}`; omit to cover every project.
        #[arg(long)]
        project: Option<String>,
        /// Report what would change and write nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(clap::Args, Debug)]
pub struct ScopedName {
    /// `{owner}/{project}`.
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub name: String,
}

impl ScopedName {
    fn split(&self) -> Result<(&str, &str)> {
        let Some((owner, project)) = self.project.split_once('/') else {
            bail!("--project must be owner/project, got {:?}", self.project);
        };
        Ok((owner, project))
    }
}

/// §8.2 secrets — encrypted with the platform age public key; values are
/// write-only from here (only the dispatcher decrypts).
#[derive(clap::Subcommand, Debug)]
pub enum SecretCmd {
    Set {
        #[command(flatten)]
        scoped: ScopedName,
        /// Omit to read the value from stdin (avoids shell history).
        #[arg(long)]
        value: Option<String>,
    },
    List {
        #[arg(long)]
        project: String,
    },
    Delete {
        #[command(flatten)]
        scoped: ScopedName,
    },
    /// Copy a secret's stored (encrypted) value between scopes without
    /// decrypting — e.g. promoting a project secret to the platform agent
    /// scope: `secret copy --from acme/demo --to global/agents --name TOKEN`.
    Copy {
        /// Source `{owner}/{project}` scope.
        #[arg(long)]
        from: String,
        /// Destination `{owner}/{project}` scope (`global/agents` = injected
        /// into every agent container, provider-credential names only).
        #[arg(long)]
        to: String,
        #[arg(long)]
        name: String,
    },
}

/// §8.1 vars — plaintext KV, injected as env by name.
#[derive(clap::Subcommand, Debug)]
pub enum VarCmd {
    Set {
        #[command(flatten)]
        scoped: ScopedName,
        #[arg(long)]
        value: String,
    },
    List {
        #[arg(long)]
        project: String,
    },
    Delete {
        #[command(flatten)]
        scoped: ScopedName,
    },
}

/// §8.3 cloud identities — plaintext KV, the record a job type's
/// `workload_identities: [name]` resolves to (design #313 A5). Not secrets:
/// there is deliberately no `copy` verb and no path to the `global/agents`
/// scope.
#[derive(clap::Subcommand, Debug)]
pub enum CloudIdentityCmd {
    Set {
        #[command(flatten)]
        scoped: ScopedName,
        /// The provider audience the minted token is valid at.
        #[arg(long)]
        audience: String,
        /// The service account the exchanged token impersonates.
        #[arg(long)]
        service_account: String,
        /// Optional per-identity cap on the minted token's lifetime.
        #[arg(long)]
        token_ttl_secs: Option<u64>,
    },
    /// Names and values — cloud coordinates are not sensitive.
    List {
        #[arg(long)]
        project: String,
    },
    Delete {
        #[command(flatten)]
        scoped: ScopedName,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum UserCmd {
    Create {
        #[arg(long)]
        email: String,
        #[arg(long)]
        password: String,
        /// Grant platform_admin.
        #[arg(long)]
        admin: bool,
    },
    List,
    Delete {
        #[arg(long)]
        email: String,
    },
    /// Mint a bearer token for an existing user (§7.1): the same JWT a login
    /// issues, signed with `jwt_private.pem`, printed to stdout. For machine
    /// callers (`Authorization: Bearer <token>`) — e.g. a Claude session
    /// driving the API. The token carries the user's roles at mint time.
    Token {
        #[arg(long)]
        email: String,
        /// Lifetime, e.g. `720h` (30 days). Shared duration syntax (§1.1).
        #[arg(long, default_value = "720h")]
        ttl: String,
    },
    /// Grant, list, or remove a user's project roles (§7.5). Writes the
    /// `project_roles` map on the user record — the same `users.*` bucket
    /// `user create` seeds.
    #[command(subcommand)]
    Role(RoleCmd),
}

#[derive(clap::Subcommand, Debug)]
pub enum RoleCmd {
    /// Grant/update a user's role on a project.
    Set {
        #[arg(long)]
        email: String,
        /// `{owner}/{project}`.
        #[arg(long)]
        project: String,
        /// `owner` | `member` | `viewer` (`owner` is the top project role, §7.5).
        #[arg(long)]
        role: String,
    },
    /// List a user's project roles.
    List {
        #[arg(long)]
        email: String,
    },
    /// Remove a user's role on a project.
    Remove {
        #[arg(long)]
        email: String,
        /// `{owner}/{project}`.
        #[arg(long)]
        project: String,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum ProjectCmd {
    Create {
        #[arg(long)]
        owner: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "main")]
        default_branch: String,
        /// Path for bare git repos (must match the dispatcher's REPOS_ROOT).
        #[arg(long, default_value = "/data/repos")]
        repos_root: PathBuf,
        /// Path baked into the pre-receive hook (§5.2). Defaults to this
        /// binary — override when the SSH host sees the artifact elsewhere
        /// (e.g. `/usr/local/bin/chuggernaut` inside the sshd container).
        #[arg(long)]
        hook_bin: Option<PathBuf>,
    },
    /// Link an existing external repo (GitHub) as a new project: the
    /// dispatcher fetches the origin, creates the `integration` branch, and
    /// seeds the chuggernaut config. Requires a running dispatcher (it holds
    /// the deploy key) and the `CHUG_ORIGIN_DEPLOY_KEY` / `CHUG_ORIGIN_PAT`
    /// project secrets — set them first with `admin secret set`.
    Link {
        #[arg(long)]
        owner: String,
        #[arg(long)]
        name: String,
        /// Origin git URL, e.g. `ssh://git@github.com/acme/api.git`.
        #[arg(long)]
        origin_url: String,
        /// Origin default branch; autodetected from the origin when omitted.
        #[arg(long)]
        main_branch: Option<String>,
    },
    List {
        #[arg(long)]
        owner: Option<String>,
    },
}

pub async fn run(args: AdminArgs) -> Result<()> {
    if let AdminCmd::WorkerCreds { node, out } = &args.cmd {
        return mint_worker_creds(&args.keys_dir, node, out.as_deref()).await;
    }
    if let AdminCmd::WorkerGitKey {
        node,
        project,
        out_dir,
        days,
    } = &args.cmd
    {
        return mint_worker_git_key(&args.keys_dir, node, project, out_dir.as_deref(), *days).await;
    }
    let store = match tokio::fs::read_to_string(args.keys_dir.join("dispatcher.creds")).await {
        Ok(creds) => NatsStore::connect_with_creds(&args.nats_url, &creds).await?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            NatsStore::connect(&args.nats_url).await?
        }
        Err(e) => bail!("reading dispatcher.creds: {e}"),
    };
    match args.cmd {
        AdminCmd::User(cmd) => run_user(&store, &args.keys_dir, cmd).await,
        AdminCmd::Project(cmd) => run_project(&store, cmd).await,
        AdminCmd::Secret(cmd) => run_secret(&store, &args.keys_dir, cmd).await,
        AdminCmd::Var(cmd) => run_var(&store, cmd).await,
        AdminCmd::CloudIdentity(cmd) => run_cloud_identity(&store, cmd).await,
        AdminCmd::WorkerRefresh {
            node,
            sha,
            tag,
            wait_secs,
            cancel,
        } => {
            if cancel {
                run_worker_refresh_cancel(&store, &node, &sha).await
            } else {
                run_worker_refresh(&store, &node, &sha, &tag, wait_secs).await
            }
        }
        AdminCmd::BackfillTaskTime { project, dry_run } => {
            run_backfill_task_time(&store, project.as_deref(), dry_run).await
        }
        AdminCmd::WorkerCreds { .. } | AdminCmd::WorkerGitKey { .. } => {
            unreachable!("handled before connect")
        }
    }
}

/// Stamp `task_time_ms` onto the terminal job records that predate the field.
/// Reuses the dispatcher's summing rule verbatim (`types::task_time_ms`) so
/// there is one implementation of "what a job's task time is", and reads one
/// job's tasks at a time — never the whole tasks bucket, which is the scan that
/// took prod down in #290.
async fn run_backfill_task_time(
    store: &NatsStore,
    project: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    let jobs = store.jobs().await?;
    let tasks = store.tasks().await?;
    let records = match project {
        Some(slug) => {
            let Some((owner, name)) = slug.split_once('/') else {
                bail!("--project must be owner/project, got {slug:?}");
            };
            jobs.list(owner, name).await?
        }
        None => jobs.list_all().await?,
    };

    let (mut stamped, mut unchanged, mut live) = (0usize, 0usize, 0usize);
    for mut job in records {
        if !job.state.is_terminal() {
            live += 1;
            continue;
        }
        let Some((owner, name)) = job.project.split_once('/') else {
            bail!(
                "job record {} has a malformed project {:?}",
                job.id,
                job.project
            );
        };
        let task_time_ms = types::task_time_ms(&tasks.list_for_job(owner, name, job.id).await?);
        if job.task_time_ms == task_time_ms {
            unchanged += 1;
            continue;
        }
        println!(
            "{}#{}: {:?} -> {:?}",
            job.project, job.id, job.task_time_ms, task_time_ms
        );
        job.task_time_ms = task_time_ms;
        if !dry_run {
            jobs.put(&job).await?;
        }
        stamped += 1;
    }
    println!(
        "{} {stamped} job(s); {unchanged} already correct, {live} live (owned by the dispatcher)",
        if dry_run { "would stamp" } else { "stamped" }
    );
    Ok(())
}

/// Prefix of the single-line detail the CLI prints on a FAILED refresh, for
/// `update.sh` to harvest into the deploy leg's `detail` field (deploy #212).
const REFRESH_DETAIL_MARKER: &str = "worker-refresh-detail:";

/// Flatten a captured tail to one line (collapse whitespace/newlines) and cap it
/// so the marker line stays a single, bounded log line. `update.sh` bounds it
/// again before it reaches the job record.
fn one_line(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(500).collect()
}

/// Request a worker self-refresh and report the outcome (spec §3.1). Soft by
/// design: it prints `refresh OK` / a `WARNING:` line and always returns `Ok`,
/// so a lagging or unreachable node never fails the deploy — the drift is
/// surfaced (here and via the fleet snapshot's per-node version), not fatal.
async fn run_worker_refresh(
    store: &NatsStore,
    node: &str,
    sha: &str,
    tag: &str,
    wait_secs: u64,
) -> Result<()> {
    use store::worker::WorkerRpc;
    use types::worker::RefreshRequest;

    let rpc = WorkerRpc::new(store.clone(), node);
    let req = RefreshRequest {
        sha: sha.to_string(),
        tag: tag.to_string(),
    };
    let ok = match rpc.refresh(&req).await {
        Ok(ok) => ok,
        Err(e) => {
            println!("WARNING: worker refresh node={node} not accepted (drift remains): {e}");
            return Ok(());
        }
    };
    if let Some(reason) = &ok.skipped {
        println!("node {node}: refresh SKIPPED — {reason}");
        return Ok(());
    }
    if !ok.accepted {
        println!(
            "refresh already in progress: node={node} from={}",
            ok.from_version
        );
        return Ok(());
    }
    println!(
        "refresh requested: node={node} from={} -> sha={sha} tag={tag}",
        ok.from_version
    );

    if wait_secs == 0 {
        return Ok(());
    }
    worker_refresh_wait(&rpc, node, sha, wait_secs).await
}

/// Cancel an in-flight refresh (ticket #254). Soft like every other refresh
/// path: it prints one machine-readable line and returns `Ok`, because the
/// deploy is ALREADY failing when this runs — a cancel that cannot be delivered
/// must not replace the real failure with its own.
///
/// The printed line is what `update.sh` folds into the cancelled node's leg
/// detail, so both outcomes are stated plainly: the refresh was stopped, or the
/// node had already swapped and stays on the new images ahead of a dispatcher
/// that will not advance (spec §3.1 version-skew window).
async fn run_worker_refresh_cancel(store: &NatsStore, node: &str, sha: &str) -> Result<()> {
    use store::worker::WorkerRpc;
    use types::worker::RefreshCancelRequest;

    let rpc = WorkerRpc::new(store.clone(), node);
    let req = RefreshCancelRequest {
        sha: sha.to_string(),
    };
    match rpc.refresh_cancel(&req).await {
        Ok(ok) if ok.cancelled => println!("refresh cancelled: node={node} sha={sha}"),
        Ok(ok) => println!(
            "refresh cancel declined: node={node} — {}",
            one_line(&ok.note)
        ),
        Err(e) => println!("refresh cancel not delivered: node={node} — {e}"),
    }
    Ok(())
}

/// Wait for a requested refresh to land, RELAYING the node's live progress to
/// stdout as it goes (ticket #253). Every line printed here rides the deploy's
/// ssh session straight into the deploy job's task output, so an operator
/// watching the job sees which image leg is building and for how long — the
/// silent multi-minute window is what this loop exists to remove.
///
/// Confirmation semantics are unchanged (#186/#187): only a `refresh OK:` line
/// means the swap landed, and the CLI always returns `Ok` — the deploy decides.
async fn worker_refresh_wait(
    rpc: &store::worker::WorkerRpc,
    node: &str,
    sha: &str,
    wait_secs: u64,
) -> Result<()> {
    use types::worker::{
        REFRESH_HEARTBEAT_SECS, RefreshConfirmation, RefreshOutcome, RefreshProgress,
        RefreshRelayState,
    };
    let started = std::time::Instant::now();
    let deadline = started + std::time::Duration::from_secs(wait_secs);
    let mut relay = RefreshRelayState::default();
    let mut last: Option<RefreshProgress> = None;
    loop {
        if let Ok(ping) = rpc.ping().await {
            if let Some(progress) = ping.refresh_progress.filter(|p| p.to_sha == sha) {
                let elapsed = started.elapsed().as_secs();
                if let Some(line) = progress.relay(sha, &mut relay, elapsed, REFRESH_HEARTBEAT_SECS)
                {
                    println!("refresh progress: node={node} {line}");
                }
                last = Some(progress);
            }
            match RefreshOutcome::confirm(sha, &ping.version, ping.refresh_outcome.as_ref()) {
                RefreshConfirmation::Confirmed => {
                    println!(
                        "refresh OK: node={node} version={} ({}s)",
                        ping.version,
                        started.elapsed().as_secs()
                    );
                    return Ok(());
                }
                RefreshConfirmation::Failed { stage, error_tail } => {
                    worker_refresh_report_failed(node, &stage, &error_tail);
                    return Ok(());
                }
                RefreshConfirmation::Pending => {}
            }
        }
        if std::time::Instant::now() >= deadline {
            worker_refresh_report_timeout(node, wait_secs, last.as_ref());
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

/// Report a refresh the node itself declared FAILED: the daemon-captured tail
/// plus the machine-readable detail line `update.sh` harvests into the leg.
fn worker_refresh_report_failed(node: &str, stage: &str, error_tail: &str) {
    println!(
        "WARNING: worker refresh node={node} FAILED at {stage} \
         (prod stays on the old images; check the fleet snapshot)"
    );
    if !error_tail.trim().is_empty() {
        println!("--- worker-refresh.sh tail (node={node}, stage={stage}) ---");
        for line in error_tail.lines() {
            println!("  {line}");
        }
        println!("--- end worker-refresh.sh tail ---");
    }
    println!(
        "{REFRESH_DETAIL_MARKER} {}",
        one_line(&format!("{stage}: {error_tail}"))
    );
}

/// Report a refresh that never confirmed within the wait window. The node
/// declared no verdict (it is probably still building), so the LAST PROGRESS we
/// relayed is the whole diagnosis — print it here too, and fold it into the
/// `worker-refresh-detail:` line, so the failing deploy leg carries "stuck at
/// build-image 3/3 agent-rust" instead of a bare "not confirmed" that sends the
/// operator ssh'ing the node (ticket #253).
fn worker_refresh_report_timeout(
    node: &str,
    wait_secs: u64,
    last: Option<&types::worker::RefreshProgress>,
) {
    println!(
        "WARNING: worker refresh node={node} not confirmed within {wait_secs}s \
         (build may still be running; check the fleet snapshot for its version)"
    );
    let Some(progress) = last else {
        println!(
            "{REFRESH_DETAIL_MARKER} not confirmed within {wait_secs}s (no progress reported)"
        );
        return;
    };
    println!(
        "--- last progress (node={node}, phase={}) ---",
        progress.phase
    );
    for line in &progress.recent {
        println!("  {line}");
    }
    println!("--- end last progress ---");
    println!(
        "{REFRESH_DETAIL_MARKER} {}",
        one_line(&format!(
            "not confirmed within {wait_secs}s; stuck at phase={} ({}s in phase); last: {}",
            progress.phase,
            progress.phase_secs,
            progress.recent.last().map(String::as_str).unwrap_or("-")
        ))
    );
}

/// Mint a worker daemon's `.creds` from the platform account seed. Non-expiring
/// like `dispatcher.creds` — rotation is re-mint + restart.
async fn mint_worker_creds(
    keys_dir: &std::path::Path,
    node: &str,
    out: Option<&std::path::Path>,
) -> Result<()> {
    if node.is_empty()
        || !node
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("--node {node:?} must be [A-Za-z0-9_-]+ (rides in NATS subjects)");
    }
    let seed_path = keys_dir.join("nats_account.seed");
    let seed = tokio::fs::read_to_string(&seed_path)
        .await
        .with_context(|| format!("reading {}", seed_path.display()))?;
    let signer = auth::nats::NatsUserSigner::from_account_seed(seed.trim())?;
    let creds = signer.mint_creds(
        &format!("worker.{node}"),
        &auth::nats::worker_permissions(node),
        None,
    )?;
    let path = out
        .map(PathBuf::from)
        .unwrap_or_else(|| keys_dir.join(format!("worker-{node}.creds")));
    crate::keygen::write_key(&path, &creds, true).await?;
    println!("wrote {} (user worker.{node})", path.display());
    Ok(())
}

/// Mint a worker node's read-only git credential (spec §3.1 self-refresh
/// enrollment): generate a keypair, sign a long-lived READ-ONLY cert scoped to
/// the platform repo, and write `worker_git` + `worker_git-cert.pub` for the
/// operator to install into the node's credential directory
/// (deploy/prod/README.md §6). Local-only: signs with the `ssh_ca` key, no NATS.
async fn mint_worker_git_key(
    keys_dir: &std::path::Path,
    node: &str,
    project: &str,
    out_dir: Option<&std::path::Path>,
    days: i64,
) -> Result<()> {
    if node.is_empty()
        || !node
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("--node {node:?} must be [A-Za-z0-9_-]+ (rides in NATS subjects)");
    }
    let Some((owner, repo)) = project.split_once('/') else {
        bail!("--project must be owner/project, got {project:?}");
    };
    if days <= 0 {
        bail!("--days must be positive, got {days}");
    }
    let ca_key = keys_dir.join("ssh_ca");
    if !ca_key.is_file() {
        bail!(
            "SSH CA key {} not found — run `chuggernaut init` first",
            ca_key.display()
        );
    }
    let cred = auth::ssh::SshCa::new(&ca_key)
        .issue_node_credential(owner, repo, node, chrono::Duration::days(days))
        .await?;

    let dir = out_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| keys_dir.into());
    let key_path = dir.join("worker_git");
    let cert_path = dir.join("worker_git-cert.pub");
    crate::keygen::write_key(&key_path, &cred.private_key, true).await?;
    crate::keygen::write_key(&cert_path, &cred.certificate, false).await?;

    println!(
        "wrote {} (read-only, {project}, {days}d)",
        key_path.display()
    );
    println!("wrote {}", cert_path.display());
    println!(
        "install both into the node's credential directory at 0600 — on Linux that is \
         /etc/chuggernaut/keys, root-owned 0700, so scp to a staging path and then \
         'sudo install -o root -g root -m 0600 <staged> /etc/chuggernaut/keys/worker_git'; \
         on macOS it is ~/chuggernaut-worker/keys/ (deploy/prod/README.md §6). Then set on \
         the worker daemon: WORKER_REFRESH_GIT_URL=ssh://git@<ssh-front>:2222/{project}.git"
    );
    Ok(())
}

async fn run_secret(store: &NatsStore, keys_dir: &std::path::Path, cmd: SecretCmd) -> Result<()> {
    use store::secrets::{AgeSecretStore, SecretStore};
    let bucket = store.raw_bucket(store::buckets::SECRETS).await?;

    if let SecretCmd::Copy { from, to, name } = &cmd {
        let split = |s: &str| -> Result<(String, String)> {
            let Some((o, p)) = s.split_once('/') else {
                bail!("scope must be owner/project, got {s:?}");
            };
            Ok((o.to_string(), p.to_string()))
        };
        let (fo, fp) = split(from)?;
        let (to_o, to_p) = split(to)?;
        let Some(value) = bucket
            .get_json::<String>(&format!("{fo}.{fp}.{name}"))
            .await?
        else {
            bail!("secret {from}.{name} not found");
        };
        bucket
            .put_json(&format!("{to_o}.{to_p}.{name}"), &value)
            .await?;
        println!("copied secret {from}.{name} -> {to}.{name}");
        return Ok(());
    }

    let public_key = tokio::fs::read_to_string(keys_dir.join("age_public.key"))
        .await
        .with_context(|| {
            format!(
                "reading {}/age_public.key (run init first)",
                keys_dir.display()
            )
        })?;
    let secrets = AgeSecretStore::for_api(bucket, public_key.trim())?;
    match cmd {
        SecretCmd::Set { scoped, value } => {
            let (owner, project) = scoped.split()?;
            let value = match value {
                Some(v) => v,
                None => {
                    let mut buf = String::new();
                    std::io::stdin().read_line(&mut buf)?;
                    buf.trim_end_matches('\n').to_string()
                }
            };
            secrets.set(owner, project, &scoped.name, &value).await?;
            println!("set secret {owner}/{project}.{}", scoped.name);
        }
        SecretCmd::List { project } => {
            let Some((owner, project)) = project.split_once('/') else {
                bail!("--project must be owner/project, got {project:?}");
            };
            for name in secrets.list(owner, project).await? {
                println!("{name}");
            }
        }
        SecretCmd::Delete { scoped } => {
            let (owner, project) = scoped.split()?;
            secrets.delete(owner, project, &scoped.name).await?;
            println!("deleted secret {owner}/{project}.{}", scoped.name);
        }
        SecretCmd::Copy { .. } => unreachable!("handled above"),
    }
    Ok(())
}

async fn run_var(store: &NatsStore, cmd: VarCmd) -> Result<()> {
    let vars = store.raw_bucket(store::buckets::VARS).await?;
    match cmd {
        VarCmd::Set { scoped, value } => {
            let (owner, project) = scoped.split()?;
            store::keys::validate_name(&scoped.name)?;
            vars.put_json(&format!("{owner}.{project}.{}", scoped.name), &value)
                .await?;
            println!("set var {owner}/{project}.{}", scoped.name);
        }
        VarCmd::List { project } => {
            let Some((owner, project)) = project.split_once('/') else {
                bail!("--project must be owner/project, got {project:?}");
            };
            let prefix = format!("{owner}.{project}.");
            for key in vars.keys_with_prefix(&prefix).await? {
                if let Some(name) = key.strip_prefix(&prefix) {
                    println!("{name}");
                }
            }
        }
        VarCmd::Delete { scoped } => {
            let (owner, project) = scoped.split()?;
            vars.delete(&format!("{owner}.{project}.{}", scoped.name))
                .await?;
            println!("deleted var {owner}/{project}.{}", scoped.name);
        }
    }
    Ok(())
}

/// §8.3 cloud identity records (design #313 A5). Operator-managed exactly as
/// secrets and vars are — CLI only, no HTTP route — and stored plaintext in
/// their own bucket, never through the secret store.
async fn run_cloud_identity(store: &NatsStore, cmd: CloudIdentityCmd) -> Result<()> {
    let bucket = store.raw_bucket(store::buckets::CLOUD_IDENTITIES).await?;
    match cmd {
        CloudIdentityCmd::Set {
            scoped,
            audience,
            service_account,
            token_ttl_secs,
        } => {
            let (owner, project) = scoped.split()?;
            keys::validate_subject_component(&scoped.name)?;
            for (field, value) in [
                ("--audience", &audience),
                ("--service-account", &service_account),
            ] {
                if value.trim().is_empty() {
                    bail!("{field} must not be empty");
                }
            }
            let record = types::CloudIdentity {
                audience,
                service_account,
                token_ttl_secs,
            };
            bucket
                .put_json(&format!("{owner}.{project}.{}", scoped.name), &record)
                .await?;
            println!("set cloud identity {owner}/{project}.{}", scoped.name);
        }
        CloudIdentityCmd::List { project } => {
            let Some((owner, project)) = project.split_once('/') else {
                bail!("--project must be owner/project, got {project:?}");
            };
            let prefix = format!("{owner}.{project}.");
            for key in bucket.keys_with_prefix(&prefix).await? {
                let Some(name) = key.strip_prefix(&prefix) else {
                    continue;
                };
                let Some(record) = bucket.get_json::<types::CloudIdentity>(&key).await? else {
                    continue;
                };
                println!("{name}\t{}\t{}", record.audience, record.service_account);
            }
        }
        CloudIdentityCmd::Delete { scoped } => {
            let (owner, project) = scoped.split()?;
            bucket
                .delete(&format!("{owner}.{project}.{}", scoped.name))
                .await?;
            println!("deleted cloud identity {owner}/{project}.{}", scoped.name);
        }
    }
    Ok(())
}

async fn run_user(store: &NatsStore, keys_dir: &std::path::Path, cmd: UserCmd) -> Result<()> {
    let users = store.raw_bucket(store::buckets::USERS).await?;
    match cmd {
        UserCmd::Token { email, ttl } => {
            let Some(user) = users.get_json::<User>(&keys::user_key(&email)).await? else {
                bail!("user {email} not found");
            };
            let ttl = types::parse_duration(&ttl)
                .map_err(|e| anyhow::anyhow!("--ttl: {e}"))
                .and_then(|d| chrono::Duration::from_std(d).context("ttl out of range"))?;
            let pem = tokio::fs::read(keys_dir.join("jwt_private.pem"))
                .await
                .with_context(|| {
                    format!(
                        "reading {}/jwt_private.pem (run init first)",
                        keys_dir.display()
                    )
                })?;
            let signer = auth::jwt::JwtSigner::from_pem(&pem)?;
            let identity = types::Identity {
                sub: user.email.clone(),
                kind: types::IdentityKind::User,
                project_roles: user.project_roles.clone(),
                platform_admin: user.platform_admin,
            };
            println!("{}", signer.issue(&identity, ttl)?);
        }
        UserCmd::Create {
            email,
            password,
            admin,
        } => {
            if !create_user(store, &email, &password, admin).await? {
                bail!("user {email} already exists");
            }
            println!(
                "created {email}{}",
                if admin { " (platform admin)" } else { "" }
            );
        }
        UserCmd::List => {
            for key in users.keys_with_prefix("").await? {
                let Some(user) = users.get_json::<User>(&key).await? else {
                    continue;
                };
                let admin = if user.platform_admin {
                    "  [platform admin]"
                } else {
                    ""
                };
                println!("{}{admin}", user.email);
            }
        }
        UserCmd::Delete { email } => {
            users.delete(&keys::user_key(&email)).await?;
            println!("deleted {email}");
        }
        UserCmd::Role(cmd) => run_role(store, cmd).await?,
    }
    Ok(())
}

/// §7.5 project-role management on the user record. Mutates the `project_roles`
/// map in the `users.*` bucket — the same write path `user create` uses; the
/// admin CLI is the operator-side writer. `--project` is `{owner}/{project}`.
async fn run_role(store: &NatsStore, cmd: RoleCmd) -> Result<()> {
    let users = store.raw_bucket(store::buckets::USERS).await?;
    let slug = |project: &str| -> Result<String> {
        let Some((owner, name)) = project.split_once('/') else {
            bail!("--project must be owner/project, got {project:?}");
        };
        keys::validate_subject_component(owner)?;
        keys::validate_subject_component(name)?;
        Ok(format!("{owner}/{name}"))
    };
    match cmd {
        RoleCmd::Set {
            email,
            project,
            role,
        } => {
            let slug = slug(&project)?;
            let Some(role) = types::ProjectRole::parse(&role) else {
                bail!("--role {role:?} must be owner|member|viewer");
            };
            let key = keys::user_key(&email);
            let Some(mut user) = users.get_json::<User>(&key).await? else {
                bail!("user {email} not found");
            };
            user.project_roles.insert(slug.clone(), role);
            users.put_json(&key, &user).await?;
            println!("granted {email} {role:?} on {slug}");
        }
        RoleCmd::List { email } => {
            let Some(user) = users.get_json::<User>(&keys::user_key(&email)).await? else {
                bail!("user {email} not found");
            };
            let mut roles: Vec<_> = user.project_roles.iter().collect();
            roles.sort_by_key(|(p, _)| p.as_str());
            for (project, role) in roles {
                println!("{project}\t{role:?}");
            }
        }
        RoleCmd::Remove { email, project } => {
            let slug = slug(&project)?;
            let key = keys::user_key(&email);
            let Some(mut user) = users.get_json::<User>(&key).await? else {
                bail!("user {email} not found");
            };
            if user.project_roles.remove(&slug).is_none() {
                bail!("user {email} has no role on {slug}");
            }
            users.put_json(&key, &user).await?;
            println!("removed {email}'s role on {slug}");
        }
    }
    Ok(())
}

/// Create a user record if absent; returns false if it already exists.
/// Safe as check-then-put: the admin CLI is the only writer of `users.*`.
pub async fn create_user(
    store: &NatsStore,
    email: &str,
    password: &str,
    admin: bool,
) -> Result<bool> {
    let users = store.raw_bucket(store::buckets::USERS).await?;
    let key = keys::user_key(email);
    if users.get_json::<User>(&key).await?.is_some() {
        return Ok(false);
    }
    let user = User {
        id: key.clone(),
        email: email.to_string(),
        password_hash: auth::hash_password(password)?,
        project_roles: Default::default(),
        platform_admin: admin,
        created_at: Utc::now(),
    };
    users.put_json(&key, &user).await?;
    Ok(true)
}

#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
async fn run_project(store: &NatsStore, cmd: ProjectCmd) -> Result<()> {
    let counters = store.raw_bucket(store::buckets::COUNTERS).await?;
    match cmd {
        ProjectCmd::Create {
            owner,
            name,
            default_branch,
            repos_root,
            hook_bin,
        } => {
            keys::validate_subject_component(&owner)?;
            keys::validate_subject_component(&name)?;
            if owner == keys::RESERVED_OWNER {
                bail!("owner {owner:?} is reserved");
            }
            let key = format!("{owner}.{name}");
            if counters.get_json::<u64>(&key).await?.is_some() {
                bail!("project {owner}/{name} already exists");
            }
            vcs::RepoManager::new(&repos_root)
                .create_project(&owner, &name, &default_branch)
                .await?;
            let bin = match hook_bin {
                Some(path) => path,
                None => std::env::current_exe()?,
            };
            crate::sshfront::install_pre_receive_hook(&repos_root, &owner, &name, &bin).await?;
            counters.put_json(&key, &0u64).await?;
            println!(
                "created {owner}/{name} (default branch {default_branch}) at {}",
                repos_root
                    .join(&owner)
                    .join(format!("{name}.git"))
                    .display()
            );
        }
        ProjectCmd::Link {
            owner,
            name,
            origin_url,
            main_branch,
        } => {
            keys::validate_subject_component(&owner)?;
            keys::validate_subject_component(&name)?;
            let needs_ssh = origin_url.starts_with("ssh://") || origin_url.contains('@');
            let is_github = origin_url.contains("github.com");
            let secrets = store.raw_bucket(store::buckets::SECRETS).await?;
            for (needed, secret) in [
                (needs_ssh, "CHUG_ORIGIN_DEPLOY_KEY"),
                (is_github, "CHUG_ORIGIN_PAT"),
            ] {
                if needed
                    && secrets
                        .get_json::<String>(&format!("{owner}.{name}.{secret}"))
                        .await?
                        .is_none()
                {
                    bail!(
                        "secret {secret} is not set for {owner}/{name} — \
                         run: chuggernaut admin secret set --scoped {owner}/{name}.{secret}"
                    );
                }
            }
            let payload = serde_json::to_vec(&serde_json::json!({
                "owner": owner, "name": name, "origin_url": origin_url,
                "main_branch": main_branch,
            }))?;
            let reply = store
                .request_with_retry(
                    &store::subjects::projects_link(),
                    &payload,
                    3,
                    std::time::Duration::from_millis(300),
                )
                .await
                .context("dispatcher unavailable (link requires a running dispatcher)")?;
            let value: serde_json::Value = serde_json::from_slice(&reply.payload)?;
            if let Some(err) = value.get("error") {
                bail!("link failed: {err}");
            }
            let main = value["origin"]["main_branch"].as_str().unwrap_or("?");
            println!("linked {owner}/{name} -> {origin_url} (origin main: {main})");
        }
        ProjectCmd::List { owner } => {
            let prefix = owner.map(|o| format!("{o}.")).unwrap_or_default();
            for key in counters.keys_with_prefix(&prefix).await? {
                if let Some((o, p)) = key.split_once('.') {
                    println!("{o}/{p}");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// `one_line` collapses a multi-line tail into a single bounded log line, so
    /// the `worker-refresh-detail:` marker `update.sh` harvests is always one
    /// line and cannot grow unbounded (deploy #212).
    #[test]
    fn one_line_flattens_and_bounds() {
        let tail = "build: docker: no space left on device\n  writing layer\n  ~11G free";
        let out = one_line(tail);
        assert!(!out.contains('\n'));
        assert_eq!(
            out,
            "build: docker: no space left on device writing layer ~11G free"
        );

        let huge = "x ".repeat(1000);
        assert!(one_line(&huge).chars().count() <= 500);
    }
}
