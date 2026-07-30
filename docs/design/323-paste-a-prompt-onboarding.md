# Design — paste-a-prompt onboarding: stand up an instance and onboard your own repo

Status: PROPOSED. Written against the tree at `470cc0c` (2026-07-30). Every
claim about current behaviour below was read out of the source or out of
[spec.md](../../spec.md); where this document and the job brief disagree, the
tree wins and the disagreement is recorded in
§[1](#1-what-the-tree-actually-says).

**The problem.** A person who has heard of Chuggernaut has no checkout, no
tailnet access, and possibly no Claude Code. They should be able to paste a
short prompt into whatever coding agent they already use and, some tens of
minutes later, be looking at a web UI where *their own repository* is a project
with a working job type, a real CI gate, and a merged first job. Today the only
supported narrator is a Claude Code skill
(`.claude/skills/chug-install/SKILL.md`), the only tested substrate is macOS,
and an imported repo ends up with no `.chug/` directory at all.

**The decision this document makes.** Keep `deploy/prod/chug-install.sh` as the
machine half and grow it; add `BOOTSTRAP.md` at the repo root as the single
agent-facing narrator, fetched once over the public mirror and thereafter read
out of the clone; and draw the shell/agent boundary as a *rule about where a
step's input comes from*, not as a list. The agent's output is always
**parameters**; the shell's output is always **state**. Everything else in this
document follows from that rule.

Related reading: [spec.md](../../spec.md) §1.1 (`.chug/` config root, job-type
schema), §4.3 (provider abstraction, permission profiles, the argv-secrets
argument), §5.3 (linked-origin projects), §6.6 (health), §12.1–12.4 (init,
project creation, admin CLI, provider defaults);
[deploy/prod/README.md](../../deploy/prod/README.md) (the manual runbook, which
stays the fallback for every stage); [INSTALL.md](../../INSTALL.md) (whose
disposition is decided in §[10](#10-surface-summary-and-the-disposition-of-installmd));
[crates.md](../../crates.md); [testing.md](../../testing.md);
[STYLE.md](../../STYLE.md) (Tier 2 rule 3, "everything is bounded", which the
runbook design leans on hard).

---

## 1. What the tree actually says

The brief names three gaps. All three are real; each is cited below with the
line that establishes it. Reading the tree turned up **four more** that change
the sizing, so they are recorded here rather than discovered by whoever
implements this.

### 1.1 The three gaps from the brief — confirmed

**Bootstrap paradox — confirmed.** `deploy/prod/README.md` §1 opens with
`git clone git@github.com:Kasofsk/chuggernaut.git "$CHUG_REPO"`, i.e. an SSH
clone of a repo a new user has no access to. `deploy/prod/update.sh`:373 reads
`CHUG_REPO="${CHUG_REPO:-$HOME/chuggernaut}"` and `:379` refuses to proceed
when that directory has no `.git`. `deploy/prod/chug-install.sh`:29 derives
`REPO` from its own location — the script cannot run before a checkout exists,
and nothing in the tree tells an outsider how to get one. This working
checkout's only `origin` is the platform's private SSH front
(`.claude/skills/chug/SKILL.md`:29).

**macOS-only substrate — confirmed, and worse than "best-effort".**
`deploy/prod/boot.sh`:17-20 calls `colima status` / `colima start`
unconditionally under `set -eu`; on a Linux host `colima` is absent, `colima
status` fails, the `!` makes the branch taken, and `colima start` exits 127 —
`boot.sh` aborts before it ever reaches `docker compose`.
`deploy/prod/install-launchd.sh` is `launchctl`/`plutil` end to end.
`deploy/prod/chug-install.sh`:146 says the systemd path is "best-effort and
UNTESTED — templates under deploy/prod/systemd (if present)" and **there is no
`deploy/prod/systemd` directory**, so on Linux `cmd_platform` prints a warning
and installs nothing. `deploy/prod/chug-mirror-install.sh`:76 guards its entire
scheduler block on `command -v launchctl`, so on Linux the mirror remote is
configured and then never pushed.

**Imported projects get no config — confirmed.**
`crates/dispatcher/src/handlers/projects.rs`:147-155 seeds `CODE_TEMPLATE` onto
the new project's default branch at create time. `deploy/prod/chug-install.sh`:200
then runs `git … push "$BARE" '+refs/heads/main:refs/heads/main'` — a **forced**
update from the imported source history, which discards that seed commit. After
`project-import` the bare repo's `main` is exactly the user's history, with no
`.chug/` anywhere in it.

### 1.2 Four more, found while reading

**(a) The platform health gate never runs.**
`deploy/prod/chug-install.sh`:152 guards on `[ -x "$HERE/deploy-health.sh" ]`,
where `$HERE` is `deploy/prod`. The script actually lives at
`.chug/tasks/deploy-health.sh` (it is a job-type evaluator — `.chug/jobs/rollback.yaml`:57).
So `cmd_platform` always takes the `else` branch and prints "no deploy-health.sh
— verify the dispatcher is up manually". The header comment at
`deploy/prod/chug-install.sh`:9 and `INSTALL.md`:26 both claim the stage
health-gates. It does not. This is a one-line fix but it invalidates the one
automated success signal the current flow claims to have, which matters a great
deal when the driver is an agent that believes what the script prints.

**(b) A fresh host has no agent image, and the seeded job type names one nobody
builds.** `crates/platform-ops/templates/code/.chug/jobs/code.yaml`:10 declares
`image: chuggernaut/agent:dev`. The only thing in the tree that builds that tag
is `deploy/dev/build.sh`:9, part of the local dev stack.
`deploy/prod/build.sh` explicitly builds *only* the ssh front and the linux
channel binary — its header says "job containers run only on worker nodes,
which build their own agent images (build-worker.sh)", and `build-worker.sh`:71
builds `chuggernaut/agent-rust:$TAG` **over ssh on a remote node**. So on the
target topology of this design — a single host, no worker — no agent image
exists on the local Docker node, and the first job to launch fails on a missing
image. Nothing in `chug-install.sh` builds one.

**(c) `host.docker.internal` cannot resolve on Linux.**
`deploy/prod/env.example`:26 and `:30` set `NATS_URL_CONTAINER` and
`REPO_URL_BASE` to `host.docker.internal`. Docker Desktop and Colima provide
that name; **Linux Docker Engine does not**, unless the container is created
with `--add-host host.docker.internal:host-gateway`.
`crates/container/src/docker.rs`:605-621 (`build_host_config`) sets only
`nano_cpus`, `memory` and an optional cache `bind` — there are no extra hosts.
So on a single-host Linux install every job container fails to reach NATS and
fails to clone the repo, with the same defaults that work on macOS. This is not
a packaging detail; it is a code change in `crates/container`, or a
config-derivation change in the install flow, or both (§[5.4](#54-addressing-on-linux-the-non-obvious-blocker)).

**(d) `chug-install.sh platform` claims an init it does not run — so a fresh
install has no keys and no admin user.** `cmd_platform` step 1
(`deploy/prod/chug-install.sh`:133-140) is commented "keys + init", logs
"generating keys + platform init", and then runs **only** `deploy/prod/boot.sh`.
`boot.sh`:16-35 is colima → `docker compose up` → `wait-nats.sh`; it invokes no
`chuggernaut` binary at all. The script header's claim to compose `chuggernaut
init` (`chug-install.sh`:9) is as wrong as the log line. The only things in the
tree that actually run init are `deploy/prod/update.sh`:546 (the deploy path,
which passes no `--admin-email`) and `deploy/prod/README.md` §1 by hand. Three
consequences for this design, which is why it is recorded here and not left to
the implementer:

- **There is no user to log in with.** §[4.4](#44-ask-versus-derive) asks the
  human for an admin email and password; without an init step consuming them,
  the flow ends at a login screen that rejects everybody, and
  §[6.2](#62-getting-a-token-and-reaching-the-ui)'s last mile cannot complete.
- **The `keys` predicate is a skip-guard in front of a step that creates no
  keys.** `chug-install.sh`:134 tests `$KEYS_DIR/jwt.pem`, but nothing in the
  taken branch writes it, so on a fresh host it stays false forever. Any
  resume rule of the form "go to the first stage that is not `ok`"
  (§[4.2](#42-checkpoints-and-resume)) would loop on `platform` indefinitely.
- **Init is genuinely two-phase, and cannot be flattened.**
  `deploy/prod/README.md`:109-110 runs it *before* the substrate is up for keys
  (with `|| true`, because the NATS leg fails with no server), and `:124-126`
  runs it *again after* `boot.sh` for topology, VAPID and the admin user. That
  ordering straddles the build/boot sequencing in
  §[5.5](#55-what-the-cold-build-does-to-the-flows-pacing), so `platform` has to
  gain two init calls around its boot, not one.

### 1.3 Two smaller facts that shape the recommendation

- **The starter template's CI evaluator is commented out.**
  `crates/platform-ops/templates/code/.chug/jobs/code.yaml`:22-24 ships the
  `ci` evaluator commented, and
  `crates/platform-ops/templates/code/.chug/tasks/ci.sh` is a stub that echoes
  "no tests configured yet" and exits 0. A freshly created project's only gate
  is the `review` agent evaluator. This is the exact thing §[8](#8-q5--chug-scaffolding-for-the-imported-repo)
  exists to fix, and it is *correctly* a stub today: nothing in the platform can
  guess a stranger's test command.
- **`spec.md` §12.2 does not mention the seed at all.** It lists four steps for
  `admin project create` (counter, bare repo, `HEAD` symref, initial empty
  commit). The code does a fifth thing — `seed_files(… CODE_TEMPLATE …)`.
  §5.3 *does* document the linked-origin `CONFIG_TEMPLATE` seed. §12.2 is behind
  the code, and this design makes that divergence worse unless it is fixed
  (§[8.6](#86-spec-consequences)).

---

## 2. The rule: where the shell/agent line falls

The brief's working hypothesis — shell owns the deterministic half, agent owns
the judgment half — is **correct, and too vague to apply to a step nobody has
thought about yet**. "Deterministic" is not a property you can check by
inspection: writing `.chug/tasks/ci.sh` is deterministic given the repo, and
choosing a data directory is a judgment call with an obvious default. Sharpen
it to a rule about *where the input comes from*:

> **The rule.** A step belongs to `chug-install.sh` if its correct behaviour is
> a function of **machine-observable state** — files on disk, process/exit
> status, an HTTP response, a git ref. A step belongs to the agent if its input
> is **outside the machine**: a human's preference, a secret only the human
> holds, or a reading of the user's repository that no fixed rule decides.
>
> **Corollary 1 (the interface).** The agent's output is *parameters* — flags,
> env-file lines, files in a staging directory. The shell's output is *state*.
> An agent never composes and runs an ad-hoc mutation; it calls a named
> subcommand with arguments. If the agent needs to run something the shell
> doesn't expose, that is a missing subcommand, not a licence to improvise.
>
> **Corollary 2 (resumability).** Every shell-owned step must be able to answer
> "what is already true?" from the machine alone, with no memory of the
> conversation. That is what makes re-run-after-half-failure work, and it is why
> the agent may be *stateless* between steps.
>
> **Corollary 3 (verification is always shell).** An agent is never asked to
> judge whether a step worked by reading output. It runs a gate that exits
> non-zero. Agents are unreliable readers of success; exit codes are not.
>
> **Corollary 4 (secrets bypass the agent entirely).** A secret travels from
> the human to the machine over stdin or a tty, never through the agent's
> context and never in argv. The agent's job is to say *which* command to run
> and *where the value goes*, not to hold the value.

Corollary 4 is not paranoia about a rogue agent; it is that agent transcripts
are logged, summarised, and sometimes pasted into issues. `spec.md` §4.3 already
makes exactly this argument for the MCP config — "inline argv leaks into `ps`,
`/proc/*/cmdline`, and crash reports" — and resolves it by writing a mode-0600
file. The install flow inherits the reasoning.

### 2.1 Applying the rule to steps that don't exist yet

The rule earns its keep on cases the current flow doesn't cover:

| New step | Input comes from | Owner | Shape |
|---|---|---|---|
| Pick the data directory | human preference, obvious default | agent asks once | `--data-dir` to `env-init` |
| Find the colima socket path | `colima status` output | shell | inside `env-init` |
| Decide platform-owned vs linked-origin | human | agent asks | `project-import` vs `project-link` |
| Get the user's repo onto disk | a git URL, then `git` exit codes | shell | `fetch-source` (§[8.3](#83-detecting-the-language-and-making-cish-real)) |
| Detect the repo's test command | reading the repo | agent proposes | edits `.chug/tasks/ci.sh` in that checkout |
| Check the test command actually passes | exit code | shell | `scaffold-verify` |
| Store the provider credential | secret the human holds | **neither** — human pipes it | agent prints the command |
| Decide `DOCKER_NODES` | `uname`, socket existence | shell | inside `env-init` |
| Decide whether to install a worker | human | agent asks | `worker-join` or skip |
| Decide the install is finished | job state via the API | shell | `smoke` / `status` exit code |

Two rows are worth dwelling on, because they are where the boundary is
genuinely interesting:

**"Detect the test command" splits across the line.** Choosing `cargo test
--workspace` over `cargo test -p foo` is judgment. Confirming that the chosen
command exits 0 in the chosen image against the user's actual checkout is
machine-observable. So the step is *both*, decomposed: the agent writes a file,
the shell executes it and returns an exit code, and the agent's only remaining
job is to react to a failure by editing the file and asking the shell again.
This is a bounded loop (STYLE Tier 2 rule 3): at most two attempts, then stop
and ask the human.

**"Store the credential" belongs to neither.** The value is the human's; the
storage command is the shell's; the agent only knows the shape. The flow is:
the agent prints

```
claude setup-token | tail -1 | "$CHUG_REPO/target/release/chuggernaut" \
  admin --keys-dir "$KEYS_DIR" secret set --project global/agents \
  --name CLAUDE_CODE_OAUTH_TOKEN
```

the human runs it, and the
agent verifies by asking the shell whether the secret *name* now exists — never
its value. `crates/api/src/routes.rs`:477 already exposes only the
`global/agents` secret **names** in the config snapshot, so this check is
available without a new endpoint.

**Always the full binary path, never `chug`.** `deploy/prod/README.md`:134-135
writes this pipeline as `… | chug admin secret set …`, but `chug` there is a
shell alias defined for that session at `README.md`:106
(`alias chug="$PWD/target/release/chuggernaut"`) — nothing in the tree installs a
`chug` binary or symlink, and `chug-install.sh` itself invokes
`"$REPO/target/release/chuggernaut"` (`:189`). A literal executor handed the
README's shorthand gets `command not found` at the one step that is designed to
be a single clean human turn. So the rule for `BOOTSTRAP.md` is: every
`chuggernaut` invocation it prints is spelled
`"$CHUG_REPO/target/release/chuggernaut"`, and the document never establishes or
relies on an alias. It is uglier, and it is the only form that survives being
pasted into a fresh shell.

### 2.2 Alternatives to this boundary, and why they lose

**All-prose (delete `chug-install.sh`, put everything in `BOOTSTRAP.md`).**
Rejected, and the brief already rules it out, but the reason is worth stating
precisely: idempotency is a *property of a program*, not of a paragraph. The
dominant failure mode here is a half-finished attempt by an unknown agent — a
`brew install` that timed out, a `cargo build` killed by a laptop lid. Recovering
from that means asking "does `$KEYS_DIR/jwt.pem` exist? does the bare repo have
a `main` ref?" and branching. Prose can *describe* those checks; it cannot make
a weak agent perform all of them in order every time. And `chug-install.test.sh`
pins two real regressions (#186 config-validation fatality, #276 `SELF_REPO`
across re-import and a failing mirror step) that a prose runbook could not have
a test for at all.

**All-shell (a `chug-install.sh wizard` that prompts for everything).**
Tempting — it would remove the agent from the critical path. Rejected because
the single most valuable thing in the flow, writing a `.chug/` config that fits
*this* repo, is not promptable. A wizard can ask "what is your test command?";
it cannot read a `Cargo.toml` with three workspace members and a `justfile` and
work out that the answer is `cargo test --workspace --no-fail-fast`. That gap is
exactly why `crates/platform-ops/templates/code/.chug/tasks/ci.sh` is a stub
today. A wizard is also worse at the failure path: an unattended prompt on a
tty is a hang, whereas an agent can read an error and ask a human a *better*
question.

**Agent-with-MCP (ship a chug-install MCP server the agent drives).** Rejected
for this milestone: it presumes the agent supports MCP, which contradicts the
agent-agnostic constraint, and it requires a running platform to talk to — which
is what we are trying to create. Worth revisiting later for the *post*-install
surface, where the platform is up and the `chuggernaut-channel` server already
exists.

---

## 3. Q1 — the prompt and the document

### 3.1 What is literally pasted

```
Set up Chuggernaut on this machine and onboard my repository.

Fetch https://raw.githubusercontent.com/kasofsk/chuggernaut/main/BOOTSTRAP.md
and follow it exactly, from the top, all the way to the end.

That document is the only source of truth. This message contains no
instructions beyond "go read it" — do not infer steps from it, and do not
substitute your own knowledge of how such systems are usually installed.
```

Three properties, each deliberate:

1. **Exactly one URL, and it is version-free.** `main` is the mirror's only
   branch and it is force-pushed roughly every five minutes, so the URL is
   stable for as long as the repo exists. No SHA, no tag, no version number the
   user would have to be given a fresh one of.
2. **No step list.** Any step named in the pasted text is a step that will rot,
   and — worse — that a literal agent will try to reconcile against the document
   when they drift. The prompt explicitly disclaims itself.
3. **An anti-improvisation clause.** The observed failure mode of a capable
   agent handed an install task is to substitute its own prior about installing
   services. The prompt has to say "don't", and the document has to say it
   again (§[4](#4-q2--writing-for-an-unknown-agent)).

### 3.2 How the agent confirms it fetched the right thing

The honest constraint: a pasted prompt cannot carry a checksum, because the
document changes and nobody will re-issue the prompt. Three options were
weighed.

**(a) Checksum in the prompt.** Rejected. It rots on the first edit to
`BOOTSTRAP.md`, and every stale copy of the prompt in a blog post or a Slack
thread becomes a hard failure with a scary-sounding cause ("integrity check
failed"), which is the worst possible first impression.

**(b) Cryptographic signature.** Rejected. There is no key to distribute to a
user who has nothing yet; solving key distribution to solve document
authenticity is disproportionate for a public read-only mirror of an open repo.

**(c) Transfer authority to the clone. Recommended.** The fetched document's
*only* substantive job is to get the repo onto the machine. Once
`$CHUG_REPO/BOOTSTRAP.md` exists, the fetched copy is discarded and every
subsequent instruction is read from the checkout. The document says so in its
own first section, and gives the agent a mechanical check:

```sh
diff -u /tmp/BOOTSTRAP.fetched.md "$CHUG_REPO/BOOTSTRAP.md" || {
  echo "the checkout's copy is newer — from here on, follow \$CHUG_REPO/BOOTSTRAP.md"
}
```

A difference is **not an error**; it is the expected outcome of the mirror
having moved on. The rule the agent is given is "the checkout wins", which is
both true and easy for a weak executor to apply. This also answers "how does the
pasted text survive the document changing under it": the pasted text only ever
has to survive long enough to name a URL that clones a repo, and the fetched
document only ever has to survive long enough to describe a `git clone`. Both are
close to immutable.

This works *because* of the build-from-source constraint. If we shipped release
artifacts, the network-fetched document would remain authoritative for the whole
flow and would need a real integrity story. Since the clone is step one anyway,
authority transfers for free. That is a genuine argument in favour of the
build-from-source decision, not just a cost of it.

### 3.3 What `BOOTSTRAP.md` carries beyond "clone, then run the script"

If it were only "clone, then run `chug-install.sh`", it would be four lines and
we would not need it. It carries seven things the shell cannot:

1. **Self-identification and the authority-transfer rule** (§3.2), plus a
   one-paragraph statement of what the user is about to end up with, which the
   agent is told to relay before doing anything. A human who pasted a prompt
   deserves to be told what is about to happen to their machine.
2. **The preconditions no script can check before it exists** — an always-on
   machine (not a laptop that sleeps mid-build), outbound network, admin rights
   to install packages, ~15 GB free.
3. **The elicitation script** (§[6](#6-q4--config-elicitation)): the questions,
   in order, with acceptable answers and what each one derives. This is the
   agent-half contract and it is the largest section.
4. **The checkpoint ladder** (§[4.2](#42-checkpoints-and-resume)): after each
   stage, the exact command that proves it, and the exact thing to do when it
   fails.
5. **The pacing contract** (§[5.5](#55-what-the-cold-build-does-to-the-flows-pacing)):
   what the cold build costs, what to tell the human, and which parts of the
   interview to run while it compiles.
6. **The scaffolding interview** (§[8](#8-q5--chug-scaffolding-for-the-imported-repo)):
   how to read the user's repo, what to propose, what to confirm.
7. **The prohibitions**: never echo a secret, never commit `chuggernaut.env`
   (it is gitignored at `.gitignore:15` — say so and check), never edit a script
   in `deploy/prod/` to work around a failure, never force-push anything without
   an explicit human "yes".

Target length ~250 lines. Longer than that and weak agents start skipping; much
shorter and it is not carrying (3) and (6).

### 3.4 Where it lives

**Repo root.** Rejected alternatives: `deploy/prod/BOOTSTRAP.md` (URL is longer
and, more importantly, invisible to someone browsing the GitHub landing page —
the mirror's front page is a real part of the funnel) and `docs/BOOTSTRAP.md`
(same visibility problem, and `docs/` is the project wiki per `spec.md` §9.4,
which is *generated* content territory). The root already holds the
orientation documents (`CLAUDE.md`, `NORTH-STAR.md`, `STYLE.md`, `INSTALL.md`),
and the raw URL `…/main/BOOTSTRAP.md` is the shortest stable thing we can print.

The name is also load-bearing in a small way: `BOOTSTRAP.md` says "this is how
you begin", where `INSTALL.md` has come to mean "the runbook for someone who
already has the tree". §[10](#10-surface-summary-and-the-disposition-of-installmd)
resolves the overlap.

---

## 4. Q2 — writing for an unknown agent

The executor may be more literal than Claude, may have a smaller context, may
not have a persistent shell, and may lose the thread entirely between steps. The
document is written for the weakest plausible executor; a strong one loses
nothing by following it.

### 4.1 Structure

- **Numbered stages, one command per step.** No "you may also want to", no
  parenthetical alternatives inline. Alternatives go in a clearly-marked
  appendix the main flow never branches into.
- **Every step states its expected observable.** Not "run preflight" but "run
  `deploy/prod/chug-install.sh preflight`; it must exit 0 and its last line must
  be `preflight OK`". A literal agent can check that; a judgment call ("make
  sure it looks right") it cannot.
- **The document tells the agent when to talk to the human.** Explicit
  `ASK THE HUMAN:` and `TELL THE HUMAN:` markers, so relaying is a step rather
  than a style choice. Weak agents narrate everything or nothing; markers fix
  both.
- **No step is longer than one command plus one check.** Composite steps are the
  ones agents half-do.

### 4.2 Checkpoints and resume

Resume is the hard part, and it needs a shell primitive that does not exist
today. Add:

```
chug-install.sh status
```

which prints one machine-readable line per stage and exits 0 only when all of
them are satisfied:

```
deps=ok env=ok keys=ok admin=ok services=running health=ok image=missing project=missing scaffold=n/a access=n/a
```

Each field is derived from the machine, never from a state file: `keys` from
`$KEYS_DIR/jwt.pem` — the same file `chug-install.sh`:134 tests today, but note
that there it is a *skip-guard in front of a step that never creates it*
(§[1.2](#12-four-more-found-while-reading)(d)), so `status` is only a usable
resume primitive once `platform` actually runs init; `admin` from `chuggernaut
admin user list` returning a `platform_admin` (the second init phase's output,
and the field that would have made gap (d) visible on the first run),
`services` from `launchctl print` / `systemctl --user is-active`, `health` from
`GET /api/v1/health` (`spec.md` §6.6 — unauthenticated by design, and the gate
requires a `200` *with* an `application/json` content-type, so the SPA fallback
cannot masquerade as health), `project` from the bare repo's `main` ref,
`scaffold` from `git -C $BARE cat-file -e main:.chug/jobs/code.yaml`.

The resume rule in `BOOTSTRAP.md` is then one sentence a literal agent can
apply: **"If you do not know what has already happened, run `chug-install.sh
status` and go to the first stage it reports as not `ok`. Never undo a stage
that reports `ok`."**

A deliberate non-goal: no install-state file. A state file is a second source of
truth that goes stale exactly when you need it (a half-finished attempt), and it
would violate Corollary 2. `status` re-derives everything, every time.

### 4.3 Idempotency and bounded retries

- Every subcommand stays idempotent, as the current four are. `status` makes
  that property *visible*, which is what turns it into a resume mechanism rather
  than a safety net.
- **At most two attempts per step, then stop and report.** Bounded by rule
  (`STYLE.md` Tier 2 rule 3). The failure mode being prevented is the agent that
  retries a `cargo build` seven times against a full disk.
- **`--dry-run` before every outward step**, which the script already supports
  globally (`chug-install.sh`:314). The document requires a dry run and a human
  "yes" before `project-import` (it force-pushes) and before `worker-join`.
- **The agent never edits `deploy/prod/*.sh`.** Stated as a prohibition. An
  agent that patches the installer to get past an error produces a machine
  nobody can support and a bug report nobody can reproduce.

### 4.4 Ask versus derive

**Ask the human** (the complete list — six questions, and the document must not
grow a seventh without a reason):

1. Which repository to onboard (git URL), and whether the platform should own
   `main` (default) or the origin should (linked-origin).
2. Which agent provider and model (`claude` | `codex`; the dispatcher refuses to
   start with no provider, `spec.md` §12.4).
3. Admin email and password. These are the answers that create the account the
   human logs in with; they are consumed by `chug-install.sh platform`'s second
   init phase (§[10.1](#101-the-subcommand-surface)) — `--admin-email` as a flag,
   the password on stdin (§[4.5](#45-secrets)) — and are **never** written into
   `chuggernaut.env`.
4. Where state lives (`CHUG_DATA`), offered with the default
   `$HOME/chuggernaut-data` and normally accepted.
5. Whether the instance should be reachable off this box (localhost-only by
   default; tailnet/IP if they say yes) — this determines `NATS_URL_CONTAINER`
   and `REPO_URL_BASE` and gates the worker question.
6. Whether they want a worker node (default: no — §[9](#9-q6--the-worker-daemon)).

Plus two **confirmations**, which are not questions but gates: before the
force-push in `project-import`, and before accepting the proposed `ci.sh`.

**Derive** (never ask): OS and architecture (`uname`), the Docker socket and
therefore `DOCKER_NODES`, `DOCKER_SLOTS` from the CPU count, `KEYS_DIR` /
`REPOS_ROOT` / `BACKUP_DEST` / `UI_ROOT` (already `${CHUG_DATA}`-relative in
`deploy/prod/env.example`:11-17), `NATS_URL`, `HOOK_BIN`, `GIT_UID`,
`CHUG_IMAGE_TAG`, `NATS_NETWORK`, `SESSION_TTL`, owner/name from the repo URL
basename (`chug-install.sh`:177-178 already does this), `SELF_REPO`
(auto-detected at `chug-install.sh`:239-259), and the repo's language and test
command (proposed, then confirmed — §[8](#8-q5--chug-scaffolding-for-the-imported-repo)).

### 4.5 Secrets

Three prohibitions, stated in the document and enforced in the shell where
possible:

- **Never echo.** The agent does not print a secret back for confirmation, does
  not include it in a summary, and does not repeat it when reporting an error.
- **Never argv.** Secrets reach `chuggernaut admin secret set` on **stdin**, the
  form `deploy/prod/README.md`:134 already uses. `spec.md` §4.3 makes the same
  argument for the MCP config. **One current exception, and it is on the
  critical path:** `chuggernaut init` takes the admin password as a flag only
  (`crates/cli/src/init.rs`:19-22; `spec.md` §12.3 documents
  `--admin-password <pass>`), so the README's bootstrap puts a real password in
  the process table and the shell history. The onboarding flow makes that worse
  by having an agent construct the command, so this design adds
  `init --admin-password-stdin` as the form `chug-install.sh platform` uses
  (a small CLI change, folded into job 1 of §[12](#12-q8--follow-on-work), with
  the flag retained for compatibility and a §12.3 line added).
- **Never commit.** `deploy/prod/chuggernaut.env` is gitignored
  (`.gitignore:15`); the document has the agent assert that with `git
  check-ignore` rather than trust it, since the user's own repo — the one being
  scaffolded — has a *different* `.gitignore` that the agent will be writing
  files near.

The strongest version of this is that the agent never receives the secret at
all: it prints the pipeline and the human runs it. Verification is by *name*,
via the config snapshot's `global/agents` secret names
(`crates/api/src/routes.rs`:477), never by value.

---

## 5. Q3 — two host substrates

### 5.1 The split rule

Branch inside a script where only a **command** differs; split into per-OS
scripts where the **artifact** differs. A plist cannot be branched into a
systemd unit — they are different files with different lifecycles — but "start
colima" versus "do nothing because dockerd is a system service" is three lines.

Applying that:

| Concern | macOS | Linux | Shape |
|---|---|---|---|
| Container runtime start | `colima start` | nothing (dockerd) | **branch** in `boot.sh` |
| Compose stack | identical | identical | shared |
| Service manager | launchd plists | systemd user units | **split**: `install-launchd.sh` + new `install-systemd.sh` |
| Mirror scheduler | launchd `StartInterval` | systemd timer | **split**, behind the same entry point |
| Dependency install | `brew` | `apt`/`dnf`/`pacman` | **neither** — print, don't run (§5.3) |
| `DOCKER_NODES` | colima socket path | unset + `DOCKER_SLOTS` | **branch** in new `env-init` |
| Container→host address | `host.docker.internal` | broken (§5.4) | **branch** + a code change |

Rejected alternative: **a full per-OS fork** (`chug-install-macos.sh` /
`chug-install-linux.sh`). It doubles the surface that drifts and doubles what
`chug-install.test.sh` must stub, for a flow whose macOS and Linux paths are
~85% identical. The current single script with `have launchctl` / `have
systemctl` probes (`chug-install.sh`:142-149) is the right shape; it is just
empty on the Linux side.

Also rejected: **"just require Docker Desktop on Linux"**, which would make
`host.docker.internal` work and skip §5.4 entirely. Rejected because a headless
Linux box — the most sensible host for an always-on instance — is precisely
where Docker Desktop is least appropriate.

### 5.2 The service-manager split

New: `deploy/prod/systemd/` holding `chuggernaut-{boot,dispatcher,api}.service`
templates (and `chuggernaut-mirror@.service` + `.timer` for the per-project
mirror), plus `deploy/prod/install-systemd.sh` mirroring `install-launchd.sh`'s
contract exactly: render templates for *this* checkout, install, reload,
idempotent, with an `uninstall` verb.

Two Linux specifics that will bite and belong in the design rather than in
whoever discovers them:

- **User units, not system units**, matching the macOS model where services run
  as user LaunchAgents so they share the user's Docker socket
  (`deploy/prod/README.md`:76-79). That requires `loginctl enable-linger $USER`
  so the units survive logout — the direct analogue of macOS's "Enable Automatic
  Login" requirement, and just as easy to forget. `install-systemd.sh` should
  run it and `status` should check it.
- **Docker group membership.** On Linux the deploy user must be in the `docker`
  group (or the socket must be otherwise accessible). This is a `sudo` action
  and a re-login, so it is a preflight *check* with a printed remedy, not
  something the installer performs.

Then a thin `chug-install.sh services` step picks by `uname` and calls one of
the two, so callers name a single entry point. `chug-mirror-install.sh` calls
the same dispatcher instead of hand-writing a plist inline at `:80-105`.

### 5.3 Dependency install: print, don't run

Preflight names what is missing and prints the exact install command for the
detected OS; the human runs it. Recommended over auto-install because package
installation needs `sudo`, is the most environment-specific step in the flow,
and is the hardest thing to undo when it is wrong. The cost is honest: it is one
more human turn, and it is the step most likely to strand a user whose distro
isn't in our table.

To keep that cost small, add `chug-install.sh deps` which prints a single
copy-pasteable line per detected platform, so the agent relays rather than
composes. And **fix the dependency list**, which is currently incomplete at
`chug-install.sh`:70-76: it checks `git`, `docker`, `node`, `age`, `curl` but
not `cargo` (the entire flow is build-from-source), not `docker buildx` or
`docker compose` (`deploy/prod/build.sh`:17 preflights buildx itself and
`boot.sh`:31 needs compose), and not `rsync` (README §1 step 2b). On macOS the
buildx/compose CLI-plugin symlinks (`deploy/prod/README.md`:63-67) are a
separate check from "is the binary installed" and preflight should make it.

### 5.4 Addressing on Linux: the non-obvious blocker

§[1.2](#12-four-more-found-while-reading)(c) is the single largest unknown in
the Linux path. Two ways to close it:

**(a) Derive a working address in `env-init`.** On Linux, set
`NATS_URL_CONTAINER` and `REPO_URL_BASE` to the Docker bridge gateway
(conventionally `172.17.0.1`, read from `docker network inspect bridge`) instead
of `host.docker.internal`. Zero code change, entirely inside the install flow.
Fragile in the ways bridge addressing is always fragile: a custom bridge, a
rootless daemon, or a firewall rule on the host will break it, and the failure
surfaces as a job container that cannot clone.

**(b) Add `extra_hosts: ["host.docker.internal:host-gateway"]` to
`build_host_config`** (`crates/container/src/docker.rs`:605). One field,
unit-testable next to the existing `host_config_without_cache_has_no_binds`
test, and it makes the macOS-derived defaults in `env.example` correct on both
platforms. It is a change to a platform crate to serve an install concern, which
deserves a moment's hesitation — but "the container's view of the host" is
genuinely the container backend's business, and `host-gateway` is a no-op on
Docker Desktop and Colima where the name already resolves.

**Recommend (b), with (a) as the fallback the installer applies when the
deployed dispatcher predates it.** (b) is smaller, testable at tier 1, and
removes a whole class of Linux-only surprises rather than papering one over. It
does mean the Linux path depends on a dispatcher change, which affects
sequencing (§[12](#12-q8--follow-on-work)).

### 5.5 What the cold build does to the flow's pacing

The honest number: a cold host does **two** full Rust compiles of this
workspace, not one.

1. `cargo build --release` on the host, for the native dispatcher/api/CLI
   (`deploy/prod/README.md` §1; `update.sh` assumes it).
2. `deploy/prod/build.sh` builds the ssh front from `deploy/dev/Dockerfile.ssh`,
   which compiles `chuggernaut` and `chuggernaut-channel` **again**, for linux,
   inside Docker, with a cold layer cache.

Plus `npm ci && npm run build` for `web/`, plus — per
§[1.2](#12-four-more-found-while-reading)(b) — an agent image build. On a Mac
mini or a mid-range Linux box, plan on **25–50 minutes of compilation**, and say
so up front rather than letting the human watch a silent terminal and conclude
it hung. Two guardrails:

- **Tell the human the number before starting**, with the two-compiles reason.
  An unexplained 40-minute wait is where people abandon.
- **Overlap it with the interview.** Nothing in the elicitation (§6) or the
  repo-reading half of the scaffolding (§8) needs a binary. So the flow is:
  preflight → `env-init` interview → **start the build** → read the user's repo
  and draft `.chug/` while it compiles → then `platform` → `project-import`.
  That turns most of the build into wall-clock the user was going to spend
  anyway.

For that to be drivable by an agent that may not have a persistent shell, the
build needs to be a subcommand with a poll: `chug-install.sh build` (starts it,
logs to a file, returns immediately) and `chug-install.sh build --wait` (blocks
until done, non-zero on failure). Both idempotent — a second `build` while one
is running attaches rather than starting a second.

Rejected accelerations: **debug builds** (the service units point at
`target/release/chuggernaut`, per `restart-verify.sh`:43 and `update.sh`, so a
debug build has to be redone), and **prebuilt binaries** (out of scope by
constraint, and the whole flow's authority model depends on the clone).

---

## 6. Q4 — config elicitation

`deploy/prod/env.example` has ~30 settings. A new user should answer for
**five** of the concerns below — of which only four become `chuggernaut.env`
lines; the credential is a NATS-KV secret and the admin pair goes to
`chuggernaut init`, so neither is ever written to the env file
(§[4.5](#45-secrets)).

| Var(s) | Disposition |
|---|---|
| `CHUG_DATA` | **Ask** (default `$HOME/chuggernaut-data`) |
| `KEYS_DIR`, `REPOS_ROOT`, `BACKUP_DEST`, `UI_ROOT` | Derive — already `${CHUG_DATA}`-relative (`env.example`:11-17) |
| `AGENT_PROVIDER_DEFAULT`, `AGENT_MODEL_DEFAULT` | **Ask** — no default is possible; the dispatcher refuses to start without a provider (`spec.md` §12.4) |
| provider credential | **Ask, but never through the agent** — a `global/agents` secret, §[4.5](#45-secrets) |
| admin email + password | **Ask** — `chuggernaut init --admin-email/--admin-password` (`spec.md` §12.1) |
| `NATS_URL`, `HOOK_BIN`, `SESSION_TTL`, `CHUG_IMAGE_TAG`, `NATS_NETWORK` | Derive — constants for a single host |
| `NATS_URL_CONTAINER`, `REPO_URL_BASE` | Derive **from the addressing answer** (§[5.4](#54-addressing-on-linux-the-non-obvious-blocker)) |
| `DOCKER_NODES`, `DOCKER_SLOTS` | Derive from `uname` + socket probe + CPU count |
| `WORKER_*` | Omit unless the worker question is answered yes (§[9](#9-q6--the-worker-daemon)) |
| `SELF_REPO` | Derive — already auto-detected (`chug-install.sh`:239) |
| `BACKUP_AGE_RECIPIENT`, `RCLONE_REMOTE`, `BACKUP_LOCAL_KEEP` | **Omit, commented** — see below |

Ports are derived, not asked: `4222` (NATS), `2222` (ssh front), `8080` (api).
Asking a new user to choose ports is a question with no good answer at minute
one; a collision is a `status` failure with a clear message and a documented
override, which is the right place to spend the complexity.

**The backup vars are a trap that must be handled.** They need a Cloudflare R2
account, which a first-time user will not have. Leaving them unset is not
neutral: `install-launchd.sh`:31 globs *every* template in
`deploy/prod/launchd/`, which includes three backup agents
(`com.chuggernaut.backup-{hourly,daily,monthly}`). A fresh install therefore
schedules three jobs that fail forever, filling `~/Library/Logs/chuggernaut/`
and teaching the user to ignore errors — the worst thing to teach in the first
hour. **Decision:** service install must skip the backup units when
`BACKUP_AGE_RECIPIENT` / `RCLONE_REMOTE` are unset, and `status` reports
`backups=not-configured` rather than treating it as a fault. The same rule
applies to the systemd side by construction.

### 6.1 The elicitation mechanism

New: `chug-install.sh env-init [--data-dir …] [--provider …] [--model …]
[--addressing localhost|<ip>] [--force]`. It copies `deploy/prod/env.example`,
substitutes the answers, derives everything else from the machine, and refuses
to overwrite an existing `chuggernaut.env` without `--force`. The agent's job is
to collect four answers and pass them as flags — Corollary 1 exactly. Neither
the credential nor the admin password appears among them: the first is piped by
the human (§2.1), the second reaches `platform` on stdin
(§[10.1](#101-the-subcommand-surface)).

This is where the `DOCKER_NODES` footgun dies. `env.example`:39-44 currently
explains at length that on macOS + colima you *must* set `DOCKER_NODES` to the
colima socket or the dispatcher exits with `Socket not found:
/var/run/docker.sock`, and that you must quote it because `|` is a shell pipe
when the file is sourced. That is three ways to fail, explained in prose, in the
first ten minutes. `env-init` reads the socket path from `colima status` /
`docker context inspect` and writes the line, quoted, or writes nothing at all
on Linux where `/var/run/docker.sock` exists.

### 6.2 Getting a token and reaching the UI

The last stage, `chug-install.sh access`:

1. Prints the UI URL — `http://localhost:8080` for the localhost default (the
   api binds `127.0.0.1:8080`, `env.example`:138-142), or the tailnet/serve
   hostname when addressing said otherwise. Remote exposure stays
   `deploy/prod/README.md` §5 territory; the default install does not touch
   Tailscale or cloudflared.
2. Mints a machine token — `chuggernaut admin --keys-dir "$KEYS_DIR" user token
   --email <admin> --ttl 720h` — into a mode-0600 file, and tells the agent the
   *path*, never the value.
3. Verifies: `GET /api/v1/health` returns `200` with `application/json`
   (`spec.md` §6.6), and one authenticated call (`GET /api/v1/projects`) returns
   the imported project.

The human logs into the UI with the email and password from `chuggernaut init` —
which is to say, from `platform`'s second init phase
(§[10.1](#101-the-subcommand-surface)); if that phase has not run, this stage has
nothing to log in with, which is gap
§[1.2](#12-four-more-found-while-reading)(d) surfacing at the last possible
moment. There is no self-service signup and none is proposed here —
`spec.md` §7.2 makes user management CLI-only, deliberately.

One documentation defect worth fixing alongside: `spec.md` §12.3's admin CLI
reference lists `user create` / `list` / `role set` / `role unset` / `delete`
but **not** `user token`, even though §6.2 (line 1583) tells machine callers to
mint one with it and `crates/cli/src/admin.rs`:208 implements it. The onboarding
flow depends on that command; §12.3 should list it.

---

## 7. Interlude — the shape of the whole flow

```
paste prompt
   │
   ├─ fetch BOOTSTRAP.md ──────────────────► authority transfers to the clone
   │     git clone https://github.com/kasofsk/chuggernaut  "$CHUG_REPO"
   │
   ├─ chug-install.sh preflight ────────────► deps + remedies (does not install)
   ├─ [interview — the six questions of §4.4]
   ├─ chug-install.sh env-init ─────────────► chuggernaut.env, fully derived
   │                                           (four answers; the admin pair and
   │                                            the credential are held back)
   ├─ chug-install.sh build  (background) ──┐
   │                                        │  ~25–50 min, two Rust compiles
   ├─ chug-install.sh fetch-source <url> ◄──┤  working checkout of the user's repo
   ├─ read that checkout, draft .chug/ ◄────┘  (agent judgment, no binary needed)
   │
   ├─ chug-install.sh build --wait
   ├─ chug-install.sh platform ─────────────► init #1 (keys, pre-NATS)
   │      --admin-email <addr>              ► boot.sh (runtime + NATS + ssh)
   │      (password on stdin)               ► init #2 (topology, VAPID, admin user)
   │                                        ► service units, then the health gate
   ├─ [human pipes the provider credential]
   ├─ chug-install.sh agent-image ──────────► an image the seeded job type names
   ├─ chug-install.sh scaffold-verify ──────► the drafted ci.sh must exit 0
   ├─ [CONFIRM] chug-install.sh project-import <url> --from <checkout>
   ├─ chug-install.sh smoke ────────────────► a command-only job that merges
   ├─ chug-install.sh access ───────────────► URL + token + authenticated probe
   └─ [optional] chug-install.sh worker-join
```

Every box on the left rail is a shell subcommand with an exit code. Every
bracketed step is the agent doing something a script cannot. That is the rule
from §2, drawn.

Two rows carry more than their width suggests:

- **`platform` is four ordered things, not one.** It runs `chuggernaut init`
  *twice* — once before the substrate for keys (tolerating the NATS leg's
  failure, `deploy/prod/README.md`:109-110) and once after `boot.sh` for
  topology, VAPID and the admin user (`:124-126`). Today it runs neither
  (§[1.2](#12-four-more-found-while-reading)(d)), which is why this box is the
  one place the diagram is describing a change rather than a rename.
- **`fetch-source` is what puts the user's repo on disk**, and it is the single
  checkout that the drafting step, `scaffold-verify` and `project-import` all
  operate on. Nothing else in the flow clones it (§[8.3](#83-detecting-the-language-and-making-cish-real)).

---

## 8. Q5 — `.chug/` scaffolding for the imported repo

This is the part that does not exist, and it is the part that decides whether
the user's first job can run.

### 8.1 Where the config commit comes from

**(a) Pre-push seed — recommended.** Build the `.chug/` commit on top of the
imported history *before* the push in `chug-install.sh project-import`, so the
platform's bare repo never has a `main` without config.

Today's mechanics cannot host that commit, in two ways. `chug-install.sh`:195-198
clones the source with `git clone --bare` into a `mktemp -d` — a bare repo has
no working tree to commit into — and `trap 'rm -rf "$TMP"' EXIT` (`:196`) deletes
it when the subcommand returns, so the checkout does not outlive the step and
cannot be read by the agent beforehand or by `scaffold-verify` afterwards. It
also happens *after* the point in the flow where the agent needs to read the
repo.

So the clone moves out of `project-import` and becomes its own subcommand:

```
chug-install.sh fetch-source <git-url> [--owner O --name N]
```

which produces (or refreshes) an ordinary working checkout at a **named,
derived, idempotent** path — `${CHUG_DATA}/import/<owner>/<name>` — with the
source's default branch checked out, and prints that path on stdout. Re-running
fetches and hard-resets rather than re-cloning; it never deletes the tree, so a
half-finished draft survives a resume. `project-import` then takes
`--from <checkout>`, skips cloning entirely, commits the working tree's `.chug/`
onto the checked-out branch, and pushes `+HEAD:refs/heads/main` into `$BARE`.

That is Corollary 1 applied to the gap the previous draft left open: "the agent
clones the repo somewhere it picks" is exactly the improvisation the rule
forbids, because cloning a URL is machine-observable state and a path the flow
must be able to re-derive on resume. It is one clone for the whole flow, not two.

**(b) Post-import commit.** Push the history, then commit `.chug/` separately
onto the bare repo's `main`. Rejected: it opens a window in which `main` exists
with no config, and a job created in that window resolves its job type at a
`base_ref` that has none (`spec.md` §1.1: the type "references
`.chug/jobs/{type}.yaml` at `base_ref`"). It also needs a second write path into
a bare repo, which is more machinery than (a), not less.

**(c) The dogfood option — the platform's own first job writes it.** Seductive
and **circular**: to run a job you need a job type, and job types are read from
the repo at `base_ref`. The repo must already carry one. Worse, the type the
platform would seed names `image: chuggernaut/agent:dev`, which nothing builds
on a fresh host (§1.2(b)) — so even a seeded type cannot launch. Rejected as the
*first* step, and **kept as the second**: once `code` works, the natural next
move is a job whose ticket is "improve `.chug/tasks/ci.sh` to cover the whole
test matrix". That is the dogfood loop closing, and it is a much better first
real job than a toy. The scaffold's job is to be the minimum that lets the loop
start, not the final config.

### 8.2 One template surface, not a third

The scaffold's *baseline* bytes must come from `crates/platform-ops/src/seed.rs`
— `CODE_TEMPLATE` / `CONFIG_TEMPLATE` — and nowhere else. Add a CLI consumer:

```
chuggernaut admin scaffold --dir <path> [--template code] [--skip-existing]
```

which materialises the embedded template into a working tree — here, the
`fetch-source` checkout, so the agent edits the scaffold in the same tree that
`scaffold-verify` runs and `project-import` commits (there is no separate
staging directory; a staging dir would be a second path to keep in sync on
resume). `chug-install.sh`
calls it; the dispatcher's `projects.rs` and `crates/dispatcher/src/forge_ingest/origin.rs` keep
calling `seed_files` with the same constants. Three consumers, one source of
bytes. A shell copy of the template files under `deploy/prod/` would be a third
*copy*, would drift, and would fail `.chug/tasks/check-duplication.sh` (jscpd at
`threshold: 0`, per `CLAUDE.md`) the moment the shapes converged.

The existing test at `crates/platform-ops/src/seed.rs`:90-104 — every seeded
path lives under `types::CONFIG_DIR` — keeps the new command honest with
`spec.md` §1.1 for free.

### 8.3 Detecting the language and making `ci.sh` real

The agent proposes; the shell proves.

`BOOTSTRAP.md` carries a detection table so a weak agent does not have to invent
one:

| Marker | Proposed `ci.sh` body | Proposed image |
|---|---|---|
| `Cargo.toml` | `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test` | rust toolchain |
| `package.json` | the `test` script as written, plus `build` if present | node |
| `pyproject.toml` / `tox.ini` | `pytest` (or the declared runner) | python |
| `go.mod` | `go test ./...` | go |
| `Makefile` with a `test` target | `make test` | repo-specific |
| a CI workflow file | **read it** — it is the best evidence of the real command | from its container/setup steps |

The last row is the most valuable and the one an agent is uniquely good at: if
the repo has a GitHub Actions workflow, the commands it runs are the answer,
already validated by the user's own history.

Then the shell proves it. New: `chug-install.sh scaffold-verify --repo
<checkout> --image <image>` runs the drafted `.chug/tasks/ci.sh` inside the
chosen image against that checkout — the one `fetch-source` produced (§8.1) — and
requires exit 0. One path, so there is nothing to keep in sync and nothing for a
resuming agent to guess. Bounded: the agent
gets two edit-and-retry attempts, then stops and asks the human (§4.3). A
`ci.sh` that has never been executed is a `ci.sh` that will fail the user's first
job, which is the worst possible moment to find out.

Two honest caveats. Verification costs a container run and a dependency install,
so it can take minutes and it can fail for reasons that are the *repo's* fault
(a test that needs a database). The document must say that a failing
`scaffold-verify` is allowed to end with "ship a `ci.sh` that runs the subset
that passes, and file a job to fix the rest" — a partial gate the user knows
about beats a stub gate they don't.

### 8.4 The image problem

`scaffold-verify` and the first job both need an image with the user's
toolchain, and §1.2(b) says none exists. Recommendation, in order of decreasing
confidence:

1. **Build a base agent image locally.** `chug-install.sh agent-image` builds
   `deploy/dev/Dockerfile.agent` as `chuggernaut/agent:local` — node 22 plus the
   Claude CLI, git, ripgrep, jq. Enough for a docs job, a JS repo, and the
   `smoke` job. This also fixes the seeded `code.yaml` naming a tag nothing
   builds; the template's default should become the tag the installer produces.
2. **For anything else, the image is the user's.** Point at
   `deploy/prod/Dockerfile.agent-rust` as the worked example of adding a
   toolchain, and have the agent offer to write a `Dockerfile` into the user's
   repo alongside the `.chug/` scaffold.
3. **Do not attempt to auto-generate a per-language agent image.** It is the
   largest remaining sharp edge in this flow and it should be named as such
   rather than half-solved. A wrong generated image fails at job-launch time
   with a container error, which is the hardest failure in the whole system for
   a newcomer to diagnose.

### 8.5 Which job types to seed

**Two working types — `code` and `docs` — plus one install-verification type,
`smoke`. Not more.**

- **`code`** — the existing template, with two changes: the `ci` evaluator
  **uncommented** (it is commented at
  `crates/platform-ops/templates/code/.chug/jobs/code.yaml`:22-24 because the
  stub would be a no-op gate; once `ci.sh` is real, a commented gate is a lie),
  and `image:` set to whatever `agent-image` produced.
- **`docs`** — agent work plus an agent review, no command evaluator. Cheap
  (needs only the base image), gives the user a second type that proves the type
  system without depending on their test suite, and matches the most common
  first real request ("write me a README section"). It should *not* ship a
  `doc-lint` equivalent: `.chug/tasks/doc-lint.sh` is this repo's, tuned to this
  repo's conventions.
- **`smoke`** — not one of the user's working types, but the install's own
  self-test: a `work: type: command` job whose evaluator is the scaffolded
  `ci.sh` (§[11](#11-q7--first-run-proof) rung 2). It has to be a *seeded* type
  and it has to **stay** in the repo, for two reasons. It cannot be supplied
  out-of-band by `chug-install.sh smoke`, because `spec.md` §1.1 resolves a job's
  type from `.chug/jobs/{type}.yaml` **at `base_ref`** — a type with no file on
  `main` is not a type. And seeding it only to delete it after rung 2 would throw
  away the thing you want on the second bad day: `smoke` is the cheapest possible
  answer to "is my instance still healthy end to end" after an upgrade, a reboot,
  or a worker joining. It costs the user one ~20-line file with a comment saying
  what it is and that deleting it is safe.

Its bytes come from the same place as everything else (§[8.2](#82-one-template-surface-not-a-third)):
a `smoke` template alongside `code` under `crates/platform-ops/templates/`,
selected with `chuggernaut admin scaffold --template smoke`. Not a heredoc in
`chug-install.sh` — that would be the third template surface §8.2 exists to
prevent, and it would be a `.chug/jobs/*.yaml` that no Rust test walks.

Rejected: `deploy`, `rollback`, `web`, `web-publish`. Every one of them encodes
*this* repo's topology — ssh into a Mini, `update.sh`, launchd, a served-UI
directory. Seeding them would hand a stranger four job types that cannot
possibly work and four files to delete before their first real job. That is the
line `smoke` sits on the right side of: it encodes no topology at all, only
"run a command in a container and merge the result", so it works on any repo on
any host from the moment it lands.

**One divergence from this repo, deliberately.** Here, the `ci` gate lives in
`.chug/jobs/_defaults.yaml` and is appended to every type. A scaffolded repo
should put `ci` in `code.yaml` only. The reason `_defaults.yaml` works here is
that `.chug/tasks/ci.sh` is diff-aware — a docs-only diff runs neither the cargo
nor the npm stage (`CLAUDE.md`, "CI — the evaluation gates ARE the CI"). A
freshly written `ci.sh` will not be diff-aware, so a project-wide gate would
make every docs job wait for the full test suite. Note this in the scaffolded
`code.yaml`'s comments so the user knows why, and what to change when their
`ci.sh` grows up.

### 8.6 Spec consequences

- **§12.2 needs a fifth step**: `admin project create` seeds `CODE_TEMPLATE`
  onto the default branch (`crates/dispatcher/src/handlers/projects.rs`:147),
  which §12.2 does not mention today. §5.3 already documents the linked-origin
  `CONFIG_TEMPLATE` seed, so §12.2 is the one that is behind.
- **§12.2 should note that an import replaces it.** With `project-import`
  force-pushing over `main`, the seeded commit survives only if the scaffold is
  carried into the import (§8.1(a)). Documenting the interaction is what keeps
  the next person from re-finding this gap.
- **§12.3** should list `admin user token` (§6.2 above), and gain
  `--admin-password-stdin` alongside the existing `--admin-password <pass>` in
  the `init` synopsis (§[4.5](#45-secrets)).
- Everything else here is consistent with §1.1's `.chug/` config root: every
  scaffolded path is under `.chug/`, which `seed.rs`'s own test enforces.

### 8.7 The ownership-model default

Keep the current default: **platform-owned + mirror**, as `INSTALL.md`:42-49
and the retired skill both describe. It is the model this repo runs on, so it is
the only one with real mileage, and it is the one where the platform can
*guarantee* the scaffold lands (it owns `main`).

The linked-origin path (`spec.md` §5.3) is offered as the answer to "I want to
keep GitHub-native PR review", with the trade-off stated in one line: the
platform proposes via `chug/release-*` PRs and never owns your default branch,
at the cost of a less-travelled code path and two `CHUG_ORIGIN_*` secrets to set
before linking. It also has a genuinely better scaffolding story —
`link_project` already seeds `CONFIG_TEMPLATE` **skip-existing** onto
`integration` (`crates/dispatcher/src/forge_ingest/origin.rs`:186-194), so the
config reaches the user as a reviewable PR. That is a nicer first experience,
and it is *not* enough to change the default, because the rest of that path has
far less mileage. Say so plainly rather than pretending the default is
uniformly better.

---

## 9. Q6 — the worker daemon

**Default: no worker, and the flow does not ask until the end.**

A single-host install runs job containers on the local Docker node — the
dispatcher's `DOCKER_NODES` entry for the local/colima socket, capped by
`DOCKER_SLOTS`. That is a complete, working topology. A worker is worth adding
when one of three things is true:

1. The host is a laptop that sleeps or moves, and the user wants jobs to keep
   running.
2. Builds are heavy enough that job containers starve the dispatcher and api on
   the same box.
3. The user needs a different architecture or OS than the host provides.

None of those is true at minute one, and adding a worker has a hard
prerequisite that a default install deliberately avoids: a remote worker
**forces** real network addressing. `deploy/prod/env.example`:24-25 and `:29-30`
spell it out — `host.docker.internal` resolves to the *worker itself* from
inside a container on the worker, so `NATS_URL_CONTAINER` and `REPO_URL_BASE`
must become a routable address. So `worker-join` is gated on the addressing
answer from §6, and offering it to a localhost-only install would be offering a
broken configuration.

`chug-install.sh worker-join` needs no changes for this flow. `BOOTSTRAP.md`
ends with a short "when to add a worker" paragraph pointing at it and at
`deploy/prod/README.md` §6. The one thing worth carrying forward is the existing
guidance the subcommand already prints (`chug-install.sh`:305-307): the node
announces itself, no dispatcher restart is needed, and the `DOCKER_NODES` slot
field is a pre-observation fallback rather than the node's capacity.

---

## 10. Surface summary and the disposition of `INSTALL.md`

### 10.1 The subcommand surface

Existing and unchanged in contract: `worker-join`. Every other existing
subcommand changes (below). Added:

| Subcommand | Owns | Why it exists |
|---|---|---|
| `status` | derive every stage's state | the resume primitive (§4.2) |
| `deps` | print per-OS install commands | so the agent relays, never composes (§5.3) |
| `env-init` | write `chuggernaut.env` from 4 answers + derivation | kills the `DOCKER_NODES` footgun (§6.1) |
| `fetch-source <url>` | a working checkout of the user's repo at a derived path | the one clone the flow makes (§8.1) |
| `build` / `build --wait` | the two Rust compiles + web + images | makes a 40-minute step pollable (§5.5) |
| `services` | dispatch to launchd or systemd | the per-OS seam (§5.2) |
| `agent-image` | build a local agent image | the seeded type names an image that exists (§8.4) |
| `scaffold-verify` | run the drafted `ci.sh`, exit non-zero | agent proposes, shell proves (§8.3) |
| `smoke` | a command-only job that merges | isolates the pipeline from the provider (§11) |
| `access` | UI URL + token + authenticated probe | the flow's last mile (§6.2) |

Changed:

- **`platform` — the largest of the three, and not a refactor.** It must
  actually run the init it already claims to (§[1.2](#12-four-more-found-while-reading)(d)),
  in two phases around the boot: `chuggernaut init --keys-dir --repos-root`
  before the compose stack (tolerating the NATS leg, as
  `deploy/prod/README.md`:109-110 does), then `boot.sh`/`wait-nats.sh`, then
  `chuggernaut init --keys-dir --repos-root --admin-email <addr>
  --admin-password-stdin` for topology, VAPID and the admin user. It therefore
  gains `--admin-email` and reads the password on stdin — the two answers from
  §[4.4](#44-ask-versus-derive) question 3, which nothing consumes today. It also
  fixes the `deploy-health.sh` path (§1.2(a)) and gains the systemd branch.
- **`project-import`** gains `--from <checkout>` and *loses* its own clone: the
  `--bare` clone into a `mktemp -d` at `chug-install.sh`:195-198 goes away in
  favour of the `fetch-source` checkout, which is where the `.chug/` commit was
  drafted and verified (§[8.1](#81-where-the-config-commit-comes-from)).
- **`preflight`** gains `cargo`, `docker buildx`, `docker compose`, `rsync`,
  docker-group and CLI-plugin-symlink checks (§5.3).

New files: `BOOTSTRAP.md` (root), `deploy/prod/install-systemd.sh`,
`deploy/prod/systemd/*.template`, `crates/platform-ops/templates/smoke/`,
`chuggernaut admin scaffold` in `crates/cli/src/admin.rs`. One new CLI flag:
`init --admin-password-stdin` (`crates/cli/src/init.rs`, §[4.5](#45-secrets)).

Deleted: `.claude/skills/chug-install/` — decided by the brief, and the point of
it. Every agent, Claude Code included, takes the `BOOTSTRAP.md` path.

The order `BOOTSTRAP.md` calls them in is the diagram in §[7](#7-interlude--the-shape-of-the-whole-flow).

### 10.2 `INSTALL.md`

Three options:

**(a) Delete it, fold everything into `BOOTSTRAP.md`.** Clean, and rejected:
`INSTALL.md` is the filename people look for in a repo, and a human who wants to
read the install path *without* an agent driving deserves a document written for
them.

**(b) Keep both as full narrators.** Rejected outright. Two documents describing
the same sequence is precisely the duplication that deleting the
`chug-install` skill was meant to end, and it would drift within one job.

**(c) Reduce `INSTALL.md` to a signpost — recommended.** ~20 lines: what
Chuggernaut is, the paste-a-prompt path (with the prompt text and a pointer to
`BOOTSTRAP.md`), the by-hand path (`deploy/prod/README.md`), and the two
ownership models in three lines. It stops being a runbook. Everything currently
in `INSTALL.md`:14-53 — the phase-by-phase commands — moves to `BOOTSTRAP.md`,
which is the only place it will be kept current, because it is the only place
anyone executes it from.

So the final shape is **one narrator** (`BOOTSTRAP.md`, for agents, and readable
by humans), **one reference** (`deploy/prod/README.md`, the manual fallback for
every stage, unchanged), and **one signpost** (`INSTALL.md`). The duplication
gate (`.chug/tasks/check-duplication.sh` at `threshold: 0`) is a useful backstop
if the signpost ever starts growing steps again.

---

## 11. Q7 — first-run proof

A single end-to-end job is the right *product* proof and the wrong *first*
diagnostic: when it fails, roughly six independent things could be the cause
(image missing, credential wrong, container→host addressing, ssh front, merge
gate, the user's own tests). So the flow proves it in three rungs, each a
subcommand with an exit code.

**Rung 1 — `chug-install.sh status`.** Services running, `GET /api/v1/health`
returns `200 application/json` (`spec.md` §6.6), the token authenticates, the
bare repo has a `main` ref carrying `.chug/jobs/code.yaml`. No containers, no
agent, seconds.

**Rung 2 — `chug-install.sh smoke`.** Creates and releases a job of the seeded
`smoke` type (§[8.5](#85-which-job-types-to-seed) — it ships in the scaffold
commit and stays there, so this rung is re-runnable after any later upgrade)
whose **work task is a command, not an agent** — legal per
the job-type schema and used in this repo already (`.chug/jobs/rollback.yaml`
declares `work: type: command`). The work step appends a line to a scratch file;
the eval step is the scaffolded `.chug/tasks/ci.sh`. It proves container launch,
placement on the local node, image resolution, the repo clone into the container
over the ssh front, NATS reachability from inside the container, the evaluator,
the squash-merge, and — on the platform-owned path — the mirror push. It costs
no provider tokens and it isolates every failure *except* the agent provider.
This rung is the new and genuinely valuable piece; without it, "the install
works" and "your API key works" fail identically.

**Rung 3 — the real `code` job.** The ticket is deliberately trivial and
deliberately *theirs*: "add a line to `README.md` describing what this
repository does". It exercises the agent provider, the permission profile
(`spec.md` §4.3 `Work`), the channel MCP `submit_result`, the review evaluator,
the CI evaluator against the real `ci.sh`, and the merge.

**What the user sees when it works.** The job page streaming the agent's output
live as it works — `spec.md` §4.3 specifies `--output-format stream-json` for
exactly this — then the review evaluator's verdict, then the CI gate, then the
job going Done, then the squash commit on `main`, and (platform-owned) that same
commit on their GitHub repo within about five minutes when the mirror agent next
fires. `BOOTSTRAP.md` should say all of that *before* rung 3 starts, so the
human knows what to watch. This is the moment the product either lands or
doesn't; it should not be narrated by an agent improvising.

---

## 12. Q8 — follow-on work

Implementation jobs in dependency order. Sizes: **S** ≈ one focused job, **M** ≈
a script plus its shell test plus doc updates, **L** ≈ likely splits under
contact.

1. **`chug-install.sh` correctness — S/M.** Two bugs and a list. Make
   `platform` run the two-phase `chuggernaut init` it claims to
   (§1.2(d)) — including `--admin-email` and the new
   `init --admin-password-stdin` (§4.5) — and fix the `deploy-health.sh` path
   (§1.2(a)); complete the preflight dependency list with `cargo`, `docker
   buildx`, `docker compose`, `rsync` and the macOS CLI-plugin symlinks
   (§5.3); gate the backup service units on their vars being set (§6). Extends
   `deploy/prod/chug-install.test.sh` — the init fix in particular wants a case
   pinning "after `platform`, `$KEYS_DIR/jwt.pem` exists and `admin user list`
   shows the admin", since its absence is what let the gap survive. Sized S/M
   rather than S because of the init work; the rest is genuinely small. No
   dependencies — land it first: until `platform` creates keys and a user,
   nothing downstream can be run end to end at all.
2. **`status` + `deps` — S/M.** The resume primitive (§4.2) and the per-OS
   remedy printer. Depends on 1 for the dependency list. Everything downstream
   uses `status`, so it comes early.
3. **`env-init` — M.** Four answers in, a fully derived `chuggernaut.env` out,
   including `DOCKER_NODES` derivation and the addressing branch (§6.1).
   Depends on 2.
4. **`build` / `build --wait` + `agent-image` — M.** The pollable build and a
   local agent image; update the starter template's default `image:` to the tag
   it produces (§5.5, §8.4). Depends on 3.
5. **`extra_hosts: host.docker.internal:host-gateway` — S.** One field in
   `build_host_config` (`crates/container/src/docker.rs`:605) plus a unit test
   (§5.4). Independent of 1–4 and can run in parallel; the Linux path depends on
   it.
6. **Linux substrate — L.** `boot.sh` runtime branch, `deploy/prod/systemd/`
   templates, `install-systemd.sh`, the `services` dispatcher, the mirror
   scheduler on systemd, `enable-linger` and docker-group handling (§5.2).
   Depends on 2 and 5. The largest and least proven item; expect it to split
   into "service units" and "mirror + boot" once someone has a real Linux box in
   front of them.
7. **`chuggernaut admin scaffold` — M.** CLI consumer of
   `crates/platform-ops/src/seed.rs` (§8.2), plus the `spec.md` §12.2/§12.3
   amendments (§8.6). Depends on nothing above; can run in parallel with 3–6.
8. **`fetch-source` + `project-import --from` — M.** One job, because it is one
   change: move the clone out of `project-import` into a `fetch-source`
   subcommand that leaves a durable working checkout at
   `${CHUG_DATA}/import/<owner>/<name>`, and have `project-import` commit that
   tree's `.chug/` and push it (§8.1). Retires the `--bare` clone and its
   `EXIT`-trap deletion at `chug-install.sh`:195-198. Depends on 7. Add a
   `chug-install.test.sh` case pinning "after import, `main` carries
   `.chug/jobs/code.yaml`" — the regression this whole section exists to
   prevent — and one pinning that a second `fetch-source` refreshes rather than
   discarding an edited tree.
9. **`scaffold-verify` — M.** Run the drafted `ci.sh` in the chosen image
   against the `fetch-source` checkout, bounded retries (§8.3). Depends on 4
   and 8.
10. **Template: uncomment `ci`, add the `docs` type — S.** In
    `crates/platform-ops/templates/` (§8.5). Depends on 9, since uncommenting
    the gate is only safe once something verifies it.
11. **`smoke`: the template and the subcommand — M.** Add
    `crates/platform-ops/templates/smoke/` (so its bytes come from the one
    template surface, §8.2), seed it with the scaffold, and add the
    `chug-install.sh smoke` subcommand that creates, releases and waits on the
    job (§11 rung 2). Depends on 8 and 10.
12. **`access` — S.** UI URL, token mint to a 0600 file, authenticated probe
    (§6.2). Depends on 2.
13. **`BOOTSTRAP.md`, `INSTALL.md` reduction, delete
    `.claude/skills/chug-install/` — M, `docs` type.** Last, because it
    describes the surface the jobs above create; writing it earlier guarantees
    it describes something that doesn't exist. Depends on 1–12.
14. **A Linux end-to-end validation run — M.** Not a code job: run 1–13 on a
    real Linux host, from the pasted prompt, with no prior checkout, and record
    what broke. Until this exists, the Linux claim is a design claim
    (§[13](#13-what-is-unproven)).

---

## 13. What is unproven

Stated plainly, because the acceptance criteria ask for it and because a
confident install document that has never been run is worse than an honest one.

- **The Linux path has never been exercised.** Not once, not partially. The
  three known blockers (colima in `boot.sh`, no systemd units, `host.docker.internal`)
  are the ones visible by reading; a first real run will find more, and the
  candidates are predictable: cgroup v2 memory limits in `build_host_config`,
  uid mapping on the bind-mounted bare repos (`compose.yaml` passes `GIT_UID`
  because Colima maps host uid through — Linux does not need the mapping but the
  ssh container's assumptions may not hold), SELinux labels on volume mounts,
  and `systemctl --user` under a session that has no lingering. Job 14 exists to
  turn that list into facts.
- **The linked-origin model is the less-travelled one.** `spec.md` §5.3 and
  `crates/dispatcher/src/forge_ingest/origin.rs` are complete, and this flow
  offers it as an option, but the mileage is in the platform-owned path. The
  divergence limitation §5.3 records ("no automated resolution" when integration
  and origin main diverge) is a real thing a new user could hit in week one.
- **`scaffold-verify` may be slow or flaky for reasons that are the repo's.** A
  test suite that needs a database or a network service will fail in a fresh
  container. The design's answer (ship the subset that passes, file a job for
  the rest) is a coping strategy, not a solution.
- **The build-time estimate is from this repo on this hardware.** 25–50 minutes
  is a Mac mini / mid-range Linux figure for a workspace this size with a cold
  Docker layer cache. A slow disk or a 2-core VM will be materially worse, and
  nothing in the flow currently measures it to find out.
- **Nobody has watched a non-Claude agent drive this.** The whole
  §[4](#4-q2--writing-for-an-unknown-agent) design — markers, exit-code
  checkpoints, bounded retries, `status`-based resume — is reasoned from how
  literal executors fail, not from a transcript of Codex or Cursor attempting
  it. The first thing job 13 should produce, after the document, is a run of it
  by an agent that is not Claude Code.
