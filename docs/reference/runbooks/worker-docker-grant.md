# Granting a worker node's docker socket to a job type

**Audience:** the prod operator. You want one node's docker daemon reachable
from the containers of **one** `(project, job type)` pair — an image build, most
likely — and nothing else on the fleet to change. This page is the whole
procedure and its failure modes.

It is *not* the decision (that is
[design #517](../../design/517-docker-access-for-jobs.md), including why the
node-root escalation is **accepted** rather than mitigated) and not the
normative text ([`docs/spec.md`](../../spec.md) §3.1). Its direct sibling is
[`worker-kvm.md`](worker-kvm.md) — a node-side capability plus a fail-closed
per-project allow-list — and the two are declared the same way, in the same
file, by the same script; [`worker-host-projects.md`](worker-host-projects.md) is
the third, and the containment this grant's acceptance leans on. For capacity see
[`worker-capacity.md`](worker-capacity.md);
for the standing deploy story, [`deploy/prod/README.md`](../../../deploy/prod/README.md) §6.

**This repo grants nothing.** No node in the fleet declares a socket and no
allow-list entry exists anywhere in the tree: picking the workloads is the
operator's act by design, which is the whole reason the mechanism is node-side
([#517 D2](../../design/517-docker-access-for-jobs.md)).

**There is one pair worth naming first, and it is a proof.**
`kasofsk/chuggernaut:docker-proof`
([`.chug/jobs/docker-proof.yaml`](../../../.chug/jobs/docker-proof.yaml),
design #517 job #538) is a command job type whose whole job is to exercise this
procedure end to end against `nuc`'s daemon and then clean up after itself — the
worked example below uses it, and it is the one command on this page that is
*meant* to be pasted. It grants node root for its run like anything else here, so
paste it as a decision and not as a demo. Everywhere else the page still uses
`<owner>/<project>` and `<job-type>` placeholders, because which of **your**
workloads gets a socket is not a question this repo may answer.

---

## 1. Read this before you grant one

A container holding the socket can bind-mount the node's filesystem, so it can
read the daemon's credential directory, take the node's NATS credential, and
subscribe the worker subject that carries **other jobs' minted credentials**.
The blast radius is the platform's execution substrate, not one project's
containers.

Design #517 D1 accepts that, on a stated condition: *every job runs code the
operator wrote or vendored*. Two consequences the procedure cannot enforce for
you:

- **Do not list a linked project whose origin main takes third-party merges.**
  `docs/spec.md` §5.3's linked-origin sync fast-forwards `integration` onto that
  origin, and `integration` is the base every job branch is cut from — so
  external code is already what such a job runs.
- **An agent type on the list means the agent holds node root** for the run.
  That is inside #517 D1's acceptance and at its widest; the first consumer
  should be a build type, which has no agent steering it.

---

## 2. Two settings, two acts, three owners

| Piece | Owned by | Where it lives |
| --- | --- | --- |
| the socket the node has | the node itself — dockerd, or colima on a mac | `/var/run/docker.sock` on Linux, `~/.colima/default/docker.sock` under colima |
| `WORKER_DOCKER_SOCKET` — that this node has one to give | `deploy/prod/build-worker.sh`; a self-refresh copies nothing forward, because the file is the declaration | the node's environment file, which its systemd unit or launchd agent hands the daemon |
| `WORKER_DOCKER_GRANTS` — **who** may hold it | the same script | the same environment file |

**The allow-list is fail-closed and the two are separate acts.** Unset or empty
grants *nobody*: a node with a socket and no entries starts, says so in its log,
and binds the socket into no launch. Enabling the capability on a node and
granting it to a workload are two decisions, exactly as `WORKER_KVM` and
`WORKER_KVM_PROJECTS` are.

An entry is `owner/project:job_type` — the `JOB_PROJECT` shape, a colon, and the
**`name:` the job type's YAML declares** (not its file stem; the two differ only
where a project lets them, and the platform already keys workload identity on
`name`). `crates/container/src/docker.rs`'s `DockerGrantEntry::parse` is the one
place that shape is spelled; `build-worker.sh` refuses at the declaration
anything that parse would refuse, so a malformed entry never reaches a node as a
grant that silently never matches.

**Renaming a job type silently revokes its grant.** That is the failure mode to
prefer — revocation, not escalation — but it is a real operational trap: an
entry naming a type nobody declares any more is indistinguishable from a working
deny.

---

## 3. The daemon's own view, and the node whose daemon is a container

The daemon refuses to **boot** when the declared socket is absent from its own
view (`crates/worker/src/daemon.rs`, `docker_grant_refusal`), after which the
supervisor restarts it into the same refusal and the node leaves the fleet. So
the question is never "does the node have a socket" but "does the process that
composes launches have one".

On a **natively supervised** daemon those coincide, and every node this script
deploys ends the run natively supervised (design #440 D1/D2). `build-worker.sh`
therefore probes the path over ssh — `[ -S <path> ]`, the node's own view — and
refuses the deploy, live daemon untouched, when it is not a socket there. That
probe is exactly the question the daemon will ask itself at boot.

On a node whose daemon is **still a container**, the socket must additionally be
mounted into `chug-worker` itself, and no script here does that: `build-worker.sh`
composes no container run spec any more (it installs the native unit and removes
any leftover `chug-worker` container in the same run), and `worker-refresh.sh`'s
swap refuses on an unconverted node rather than re-composing one. The case is
**unreachable from the deploy path, not dead** — the Mini's dispatcher and api
are native, and the nuc and the air were converted, so no live node is in it
today. If you meet one, convert it with `build-worker.sh` rather than adding a
`-v` by hand; a hand-recreated container needs
`-v <socket>:<socket>` and will boot-loop without it.

---

## 4. The procedure

**Before you start**, on the node: `ls -l <socket>` (it is a socket, and the
daemon's user must be able to open it — under colima it is mode `0600` owned by
the login user), and confirm which job type you mean by reading its `name:` out
of the project's `.chug/jobs/` file.

`build-worker.sh` needs ssh to the node, and the Mini cannot ssh a tagged worker
(Tailscale blocks tagged→tagged) — so this is a **laptop** step, exactly like
first provisioning the node:

```sh
# The worked example: the docker-proof job type on the node it is pinned to.
# Swap the entry for your own <owner>/<project>:<job-type> for anything else.
WORKER_SSH=worksalot@gumbo-nuc-0 CHUG_WORKER_NODE=nuc \
  WORKER_NATS_URL=nats://100.116.243.42:4222 WORKER_SLOTS=2 \
  WORKER_DOCKER_SOCKET=/var/run/docker.sock \
  WORKER_DOCKER_GRANTS=kasofsk/chuggernaut:docker-proof \
  deploy/prod/build-worker.sh
```

`nuc` is not a free choice in that line: `.chug/jobs/docker-proof.yaml` pins
`placement.node: nuc`, and **the pin and the entry must name each other**. An
entry on a node the job type never lands on grants nothing, and the job fails at
rung 1 having proved only that the two halves disagree.

Pass **every** var the node should keep, not just the new ones: this rewrites
the node's whole run spec, so a var you omit is a var the node loses. The
run-spec drift guard (#390) refuses rather than dropping a setting it can see
the live daemon running, but that is the reminder, not the fix.

Both settings are per node like every other one — `WORKER_DOCKER_SOCKET_<node>`
and `WORKER_DOCKER_GRANTS_<node>` in `deploy/prod/chuggernaut.env` <!-- runtime --> override
the bare names, and `deploy/prod/env.example` documents them. On prod, where deploys
reach the node over the no-ssh self-refresh path, the values in `chuggernaut.env`
never reach the daemon: the environment file the command above wrote is the
declaration, and the swap leaves it alone.

Four things `build-worker.sh` refuses before it touches the live daemon, each
because the daemon would refuse it as a hard config error or a boot failure and
the supervisor would loop that refusal:

| Refused | Because |
| --- | --- |
| a relative `WORKER_DOCKER_SOCKET` | `parse_stable_path` (`crates/worker/src/config.rs`) — a bind source the engine would resolve somewhere unintended |
| one naming a `/nix/store` hash | the same parse — a content hash goes silently wrong at the next `nixos-rebuild` |
| an entry that is not `owner/project:job_type`, or a repeated one | `DockerGrantEntry::parse` and `parse_docker_grants` — refused at the declaration rather than kept as a grant that can never match |
| a socket that is not a socket **on the node** | `docker_grant_refusal` — the boot refusal above, asked here first |

An allow-list with no socket, and a socket with an empty allow-list, are both
**warnings**: the daemon accepts each (it reads the list only through a declared
socket, and an empty list grants nobody), so the deploy says what will happen
and proceeds.

---

## 5. Verifying

```sh
# the daemon came up and says what it enabled, and what that costs
ssh worksalot@gumbo-nuc-0 journalctl -u chug-worker -b 2>&1 | grep -i docker
#   docker socket bound for the allow-listed (project, job type) pairs — each
#   holds node root for the duration …

# the declaration itself, which is the half people forget
ssh worksalot@gumbo-nuc-0 grep WORKER_DOCKER /etc/chuggernaut/worker.env

# during an allow-listed job: the job container holds the socket, at the
# conventional path a docker client looks for with no DOCKER_HOST set
ssh worksalot@gumbo-nuc-0 docker inspect <job-container> \
  --format '{{json .HostConfig.Mounts}}'
#   [{"Type":"bind","Source":"/var/run/docker.sock","Target":"/var/run/docker.sock", …}]
```

**The end-to-end check is a job, and it is written: release `docker-proof`.**
Its ladder ([`.chug/tasks/docker-proof.sh`](../../../.chug/tasks/docker-proof.sh))
reads the bind, asserts no `DOCKER_HOST` was injected, asks the daemon, builds an
image from a base the node already has, runs it, and then **removes what it made
and re-lists to prove it** — a proof that leaks an image onto a node is worse
than no proof. Its stdout is the report (`stdout.log`), and its `VERDICT` line is
last. Two failure modes read differently on purpose:

- **rung 1, socket absent** — the grant did not reach the launch. Either this
  node's daemon predates #517 S3 (job #522) or the entry above is missing or
  misspelled. It is *not* a broken docker daemon, and the message says so.
- **rung 1, the path is not a socket** — the bind exists and no client can dial
  it, which is the shape `build-worker.sh` refuses in advance (§4).

A launch that is *not* on the list gets no bind and no error until the docker
command itself fails with `Cannot connect to the Docker daemon` — loud, late, and
diagnosable only from the container log. That is the known cost of a node-side
allow-list ([#517 C3](../../design/517-docker-access-for-jobs.md));
`NodeCapabilities.docker_reachable` (design #517 S4) is what makes the node's
half of it auditable from the fleet view.

**Read the `identity` evaluator's log too.** The allow-list is matched on
`(JOB_PROJECT, JOB_TYPE)`, and an **evaluator** launch carries both stamps — so
an evaluator of an allow-listed job type may hold the socket as well, including
the appended `ci` one. `docker-proof`'s stage-0 `identity` evaluator reports
which it got and always passes; whether that is intended is design #517's
question, not this page's.

---

## 6. Turning it off, and narrowing it

Recreate the daemon without the settings (same command, those lines dropped).
Narrowing beats removing: drop the entry from `WORKER_DOCKER_GRANTS` and that
workload's launches stop receiving the socket while the node keeps working, and
drop `WORKER_DOCKER_SOCKET` to take the whole capability off the node. Both take
effect at the next daemon start, which the deploy performs.

Withholding **host-mode** docker is not on this page and is not available: a
host task reaches the daemon by the uid it runs as, so no chug setting grants or
withholds it. That is design #517 D4, and its enforcement half (per-task users)
is deferred as S6.

---

## 7. Troubleshooting

| Symptom | What it means | What to do |
| --- | --- | --- |
| the node vanishes from the fleet right after a docker-grant change, the supervisor shows the daemon restarting | the daemon is refusing to start — its log names `WORKER_DOCKER_SOCKET names …, which this daemon's own view does not have` | on a native daemon the path is wrong or gone: read the node's real one with `docker context inspect` and redeclare it. On a container daemon it is the missing mount (§3) — convert the node |
| `build-worker: WORKER_DOCKER_SOCKET='…' is not an absolute host path`, or `names a nix store path` | the value would be refused by the daemon's own parse, so the script refused before touching the live daemon | declare the stable absolute path, with no `unix://` prefix — that prefix belongs to `WORKER_DOCKER_ENDPOINT`, which is a different setting |
| `build-worker: WORKER_DOCKER_GRANTS entry '…' is not an owner/project:job_type pair` | the entry would be refused at parse and the whole allow-list with it | write `owner/project:job_type` — one slash, one colon, the job type's declared `name:` |
| `build-worker: … that is not a socket on <node>` | the path is absent, or is a regular file — the second boots the daemon and then hands every granted launch a bind no client can dial | fix the path (§4); the deploy refused with the live daemon still running |
| the daemon logs `WORKER_DOCKER_SOCKET is set but WORKER_DOCKER_GRANTS is empty` | the capability is on and granted to nobody — fail-closed, working as intended | add the entry and recreate the daemon |
| a granted job still fails with `Cannot connect to the Docker daemon` | its `(project, job type)` did not match: the allow-list is matched on `JOB_PROJECT` **and** `JOB_TYPE`, both exactly | compare the entry against the job's project and the type's declared `name:`; a renamed type revokes its own grant (§2) |
| a job that should *not* have the socket has it, in host mode | host access is ambient — the task's uid owns the socket, and nothing granted it | nothing to change here; this is design #517 D4, measured and accepted |
