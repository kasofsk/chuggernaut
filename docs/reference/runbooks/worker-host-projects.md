# Declaring which projects a host node runs work for

**Audience:** the prod operator. You are bringing up — or keeping up — a node
that serves `host` launches, and you have to say **whose** host work it runs.
This page is the whole procedure and its failure modes.

It is *not* the decision (that is
[design #309 §10](../../design/309-host-native-execution.md#10-trust-and-tenancy),
including why single-tenancy is the honest form of accepting host mode's blast
radius) and not the normative text ([`docs/spec.md`](../../spec.md) §3.1). Its
direct siblings are [`worker-kvm.md`](worker-kvm.md) and
[`worker-docker-grant.md`](worker-docker-grant.md) — a node-side capability plus
a fail-closed allow-list, declared in the same file by the same script. For
capacity see [`worker-capacity.md`](worker-capacity.md); for the standing deploy
story, [`deploy/prod/README.md`](../../../deploy/prod/README.md) §6.

**This repo declares nothing.** No `chuggernaut.env` in the tree names a
tenancy, and picking the projects is the operator's act by design. Everything
below uses `<owner>/<project>` placeholders.

---

## 1. What the list decides, and what it does not

`WORKER_HOST_PROJECTS` is a comma-separated `owner/project` allow-list. A
**host** launch whose `JOB_PROJECT` it does not name is refused at the node with
a hard launch error naming the project and the node — never `NoCapacity`,
because a refusal that can only clear by a config change on the node must not sit
in a queue pretending it might.

Three things it deliberately does not touch:

- **Container launches on the same node.** The list is read only where a host
  launch is admitted, so a mixed-mode node keeps serving container work for every
  project exactly as it did. A container-only node never constructs the backend
  that holds it.
- **Placement.** The dispatcher does not read the list; it places by
  `runtime.mode` and the node's advertised capabilities. A host launch sent to a
  node that does not list the project fails there rather than being routed
  elsewhere, which is why the list belongs on every host node in a fleet that has
  more than one.
- **What a job type may ask for.** A job type cannot request host tenancy, and
  adding a field for it is refused on sight — the node's consent is the whole
  mechanism ([#309 §10](../../design/309-host-native-execution.md#10-trust-and-tenancy)).

**Unset runs host work for nobody.** That is the same posture
`WORKER_KVM_PROJECTS` and `WORKER_DOCKER_GRANTS` take, and it is why declaring
the list is a step in bringing up a host node rather than an optional hardening
pass.

---

## 2. Why single-tenant at all

A host task gets the machine: the task user's home, every cache under it, the
node's process table, and whatever the last task left behind. That persistence
**is** the win — a warm Xcode derived-data tree, a warm gradle cache — and it is
the same property that makes contamination between projects possible. Per-task
users ([#309 §8](../../design/309-host-native-execution.md), still open) bound
*accidental* cross-reading between concurrent tasks; nothing short of a VM per
task bounds a hostile or compromised project, which is the isolation host mode
exists to give up.

So: one node, one tenant, said out loud and enforced at the node. A project's
own code persisting across its own tasks is the feature.

---

## 3. The procedure

`build-worker.sh` rewrites the node's whole run spec, so this is the same
laptop-side command that provisions any other node property — the Mini cannot ssh
a tagged worker:

```sh
WORKER_SSH=op@gumbo-air-0 CHUG_WORKER_NODE=air \
  WORKER_NATS_URL=nats://100.116.243.42:4222 \
  WORKER_MODES=container,host WORKER_SLOTS=1 WORKER_SLOTS_MAX=1 \
  WORKER_HOST_ROOT=/Users/op/chuggernaut-worker/host-tasks \
  WORKER_HOST_PROJECTS=<owner>/<project> \
  deploy/prod/build-worker.sh
```

Pass **every** var the node should keep, not just the new one: a var you omit is
a var the node loses. The run-spec drift guard (#390) refuses rather than
dropping a setting it can see the live daemon running, but that is the reminder,
not the fix.

It is per node like every other one — `WORKER_HOST_PROJECTS_<node>` in
`deploy/prod/chuggernaut.env` <!-- runtime --> overrides the bare name, and
`deploy/prod/env.example` documents it. Several projects are a comma-separated
list; **quote it** if you write spaces, so it stays one value in the operator's
own shell.

Four things the script refuses before it touches the live daemon:

| Refused | Because |
| --- | --- |
| `WORKER_MODES` naming `host` with no `WORKER_HOST_PROJECTS` | the daemon would boot, advertise its slot and refuse every host launch — a node that silently does nothing |
| a whitespace-only list | `parse_projects` (`crates/worker/src/config.rs`) trims first, so the daemon reads it as unset; refused as the empty list it is |
| an entry that is not `owner/project` | the same parse — refused at the declaration rather than kept as a tenancy that can never match a `JOB_PROJECT` |
| a repeated entry | the same parse, a hard config error there, so the supervisor would loop the refusal |

A list declared on a node that names **no** host mode is a **warning**, not a
refusal: the daemon accepts it and reads it nowhere, so the deploy says so and
proceeds.

---

## 4. Verifying

```sh
# the daemon came up and says whose host work it serves
ssh op@gumbo-air-0 log show --predicate 'process == "chuggernaut"' --last 5m \
  | grep -i host_projects
#   host execution enabled — … for the projects WORKER_HOST_PROJECTS names …

# the declaration itself, which is the half people forget
ssh op@gumbo-air-0 grep WORKER_HOST_PROJECTS /Users/op/Library/.../worker.env
```

On Linux the log is `journalctl -u chug-worker -b` and the environment file is
`/etc/chuggernaut/worker.env`.

The end-to-end check is a job: run a host job type for a listed project (it
runs), then for an unlisted one (the task fails immediately, and the error names
both the project and the node). A node whose list is empty logs its own warning
at boot — `WORKER_MODES names host and WORKER_HOST_PROJECTS is empty` — and
refuses every host launch it is handed.

---

## 5. Changing it, and turning host mode off

Re-run the command with the new list; it takes effect at the next daemon start,
which the deploy performs. Narrowing is a list with fewer entries — the removed
project's host launches start failing at once, and every other launch on the node
is untouched. Taking host mode off the node entirely means dropping `host` from
`WORKER_MODES`, at which point the tenancy decides nothing and the node stops
advertising the mode.

---

## 6. Troubleshooting

| Symptom | What it means | What to do |
| --- | --- | --- |
| `build-worker: WORKER_MODES='…' names host, but WORKER_HOST_PROJECTS is empty on <node>` | the guard this page exists for — the deploy refused with the live daemon still running | declare `WORKER_HOST_PROJECTS_<node>` and re-run (§3) |
| `build-worker: WORKER_HOST_PROJECTS entry '…' is not an owner/project pair` | the entry would be refused by the daemon's own parse, taking the whole list with it | write `owner/project` — one slash, the `JOB_PROJECT` shape |
| every host job on the node fails immediately, naming the project | the project is not on the list, or the list is empty | add it (§3); this is the enforcement working, not a fault |
| a host job fails naming `<no JOB_PROJECT>` | the launch carried no project stamp at all, which no dispatcher-composed launch does | read the daemon's log around the launch — this is a bug, not a config error |
| the daemon logs the empty-tenancy warning but the node still serves jobs | those are its **container** launches, which the list never binds | nothing to do unless you meant the node to serve host work |
| a host job runs for a project you did not list | the node is running an older daemon that predates this enforcement | re-deploy the node; `WORKER_HOST_PROJECTS` is enforced by the daemon, so an old binary ignores it |
