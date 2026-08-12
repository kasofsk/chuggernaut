# Provisioning a host node's per-project unix users

**Audience:** the prod operator, at the node with root. You have a host node
that serves more than one project — or one project plus your own login session —
and you want each project's host work to run as a **unix user of its own**
instead of as the daemon's uid. This page is the whole procedure, the order it
has to be done in, how to check it, and how to take it back.

It is *not* the decision (that is
[design #537](../../design/537-per-project-users-macos.md), including why a
`sudo` binding from the existing launchd agent beats a root daemon) and not the
normative text ([`docs/spec.md`](../../spec.md) §3.1). Its siblings are
[`worker-host-projects.md`](worker-host-projects.md) — the roster this binding is
declared *over*, and where a node's host tenancy is set —
[`worker-kvm.md`](worker-kvm.md) and
[`worker-docker-grant.md`](worker-docker-grant.md); for capacity see
[`worker-capacity.md`](worker-capacity.md), and for the standing deploy story
[`deploy/prod/README.md`](../../../deploy/prod/README.md) §6.

**Nothing on the fleet is provisioned this way yet, and the escalation has never
run from the daemon's launch path with a real project user.** `WORKER_HOST_USERS`
is off on every node, no node has the users or the `sudoers` line, and what
[#537 M1](../../design/537-per-project-users-macos.md) measured was a probe user
(`chug-probe`) on `gumbo-air-0`. So treat §4 as the part that proves it on *your*
node rather than as a formality — including the two things #537 leaves open: no
full `xcodebuild` project build has run as a project user, and M1 has never been
taken with **no console session** (an unattended reboot is unproven on that Mac).

**Everything below assumes a macOS host node**, because that is the only host
node this fleet has and the only one #537 covers: the daemon is a launchd agent
in the login user's GUI domain, so *the daemon's uid is the login user* and the
escalation goes from that user into the project users. On a Linux host node the
native unit runs the daemon as **root**, and
[#309 §8](../../design/309-host-native-execution.md#8-secrets-on-a-shared-host)'s
per-task pool with `systemd-run --uid=` is the design there — untouched by this
page, and no such node exists.

---

## 1. The order, and it is not a preference

One step here can produce a node that looks healthy and fails its next agent
launch, so the sequence is part of the procedure:

1. **Put the agent CLI at the node-wide path first** —
   `/usr/local/lib/chuggernaut/bin/claude`, as a real file, and re-render the
   daemon's plist so its `PATH` carries it. The step is in
   [`chug-node-adoption.md`](chug-node-adoption.md); design #537 slice 8 (job
   #571) moved both renderings of `AGENT_PATH` there
   (`deploy/prod/install-worker-launchd.sh` and
   `deploy/prod/build-worker.sh`'s own macOS plist).
2. Create the `WORKER_HOST_ROOT` directory with root (§3.1).
3. Create each project's **group and user, the group as the user's primary
   group** (§3.2), and put the daemon's login user in that group (§3.3).
4. Install the `sudoers` line (§3.4).
5. Only then declare `WORKER_HOST_USERS=1` and deploy (§3.5).

**Why step 1 comes first.** A host task execs the agent CLI by **bare name** off
the `PATH` it inherits from the daemon, and until #537 D12 that path's CLI
directory was under the login user's home. That home is `0750` group `staff`, and
a project user reaches it **only** while `staff` is its primary group — which is
exactly what step 3 takes away ([#537 M8](../../design/537-per-project-users-macos.md)).
Take a project user out of `staff` before the CLI is node-wide and the node keeps
advertising `agent_cli` — the daemon discovered it on its **own** `PATH` at boot
— while every agent host launch dies inside the task on a binary it cannot exec.
That is the one failure on this page that is silent at every layer above the
launch.

A symlink at the node-wide path does not count: it resolves to the same denied
path under the login user's home. Install a real file.

---

## 2. What has to exist, and who owns each piece

| Piece | Owned by | Where it lives |
| --- | --- | --- |
| the agent CLI at a node-wide path | the operator, with root (#490 D2, #537 D12) | `/usr/local/lib/chuggernaut/bin/claude` on the node |
| the host task root — node-wide, `0711`, owned by the daemon's uid | **the operator, with root**, since #537 D7 made it a precondition rather than worker-owned state | the node's filesystem; named by `WORKER_HOST_ROOT`, whose default is `/var/lib/chuggernaut/host-tasks` |
| the group `chug-{project}`, as the user's **primary** group | the operator, with root (#537 D12) | the node's directory services |
| the user `chug-{project}`, with a home | the operator, with root (#537 D9) | the node's directory services |
| the daemon's login user's membership of each project group | the operator, with root | the node's directory services |
| the `sudoers` line granting the login user `NOPASSWD` execution as exactly those users | the operator, with root (#537 C1) | `/etc/sudoers.d/chuggernaut-host-users` on the node |
| `WORKER_HOST_USERS` — that this node binds at all | `deploy/prod/build-worker.sh`; a self-refresh copies nothing forward, because the file is the declaration | the node's environment file, which its launchd agent hands the daemon |
| `WORKER_HOST_PROJECTS` — **which** projects, and therefore which users must exist | the same script ([`worker-host-projects.md`](worker-host-projects.md)) | the same environment file |

**There is no second roster.** The user name is **derived** — `chug-` (the
`USER_PREFIX` in `crates/worker/src/host_users.rs`) plus the second component of
the `owner/project` slug, so `acme/beacon` is `chug-beacon` (#537 D6). The list
of projects a node serves *is* the list of users that must exist, and nothing
maps a project to a user name a second time. Two listed projects of different
owners with the same name (`a/beacon` and `b/beacon`) derive **one** user, which
is the failure the binding exists to prevent: with the binding on, that is a hard
config error the daemon refuses to boot on and the deploy refuses first
(`enforce_user_derivation`, `crates/worker/src/config.rs`).

**The binding is fail-closed and never falls back.** A project the node declares
whose user it cannot resolve has every host launch of it refused **by name**
(`HostUsers::refusal`, `crates/container/src/host.rs`) — running it as the daemon
would look like success while restoring the shared uid the binding removes.
Container launches on the same node are untouched either way.

---

## 3. The procedure

All of §3.1–§3.4 is **on the node, with root**. `deploy/prod/build-worker.sh`
deliberately creates none of it: it runs over ssh as the login user, and #537 D9's
asymmetry means an account it created is one it could not remove.

The examples use login user `worksalot` and project `acme/beacon` ⇒ user and
group `chug-beacon`. Substitute your own.

### 3.1 The task root

The whole task tree hangs off `WORKER_HOST_ROOT`, and every project user must
**traverse** it to reach its own task directory while none may list it — so it is
`0711`, owned by the uid the daemon runs as, at a node-wide path **outside the
login user's home** (#537 D7):

```sh
sudo mkdir -p /var/lib/chuggernaut/host-tasks
sudo chown worksalot /var/lib/chuggernaut/host-tasks
sudo chmod 0711 /var/lib/chuggernaut/host-tasks
```

`HostBackend::new` (`crates/container/src/host.rs`) still does `create_dir_all`
on that root at boot, and against one that already exists that returns success
without touching it — which is what makes the move safe with no boot-path change.
`/var/lib/chuggernaut/host-tasks` is the daemon's own default, so a node using it
needs no `WORKER_HOST_ROOT` line at all; the reason a mac had to declare one
(design #322 W2 — `/var/lib` is root-owned and a non-interactive deploy has no
password) is exactly what creating it with root removes. Any other node-wide path
works the same way, declared as `WORKER_HOST_ROOT_<node>`.

**A root still inside the login user's home is warned, not refused.** It works
today, through the same `0750`-and-`staff` traversal §3.2 is about to remove — so
move it in the same visit, or the first launch after the group change fails to
create its task directory.

### 3.2 The group and the user — the group **as the primary group**

This is the step the shared `staff` primary group is the whole reason for. On
macOS a new user's primary group is `staff` by default, and `staff` is what lets
one uid traverse `/Users/worksalot` (`0750`) and read the daemon's own
`worker.env` (`0644`) — measured, [#537 M3](../../design/537-per-project-users-macos.md).
A project user merely *added* to a `chug-beacon` group beside `staff` still reads
the node's NATS URL, its declarations and every other project's roster. Set the
group as **primary** or the boundary is not there.

Pick ids nothing else uses, then create the group and the user:

```sh
# the highest id in use, so the next one is free
dscl . -list /Users UniqueID | awk '{print $2}' | sort -n | tail -1
dscl . -list /Groups PrimaryGroupID | awk '{print $2}' | sort -n | tail -1

sudo dscl . -create /Groups/chug-beacon
sudo dscl . -create /Groups/chug-beacon PrimaryGroupID 601
sudo dscl . -create /Groups/chug-beacon RealName "chuggernaut acme/beacon"

sudo dscl . -create /Users/chug-beacon
sudo dscl . -create /Users/chug-beacon RealName "chuggernaut acme/beacon"
sudo dscl . -create /Users/chug-beacon UniqueID 601
sudo dscl . -create /Users/chug-beacon PrimaryGroupID 601      # NOT staff (20)
sudo dscl . -create /Users/chug-beacon NFSHomeDirectory /Users/chug-beacon
sudo dscl . -create /Users/chug-beacon UserShell /bin/sh

sudo mkdir -p /Users/chug-beacon
sudo chown chug-beacon:chug-beacon /Users/chug-beacon
sudo chmod 0700 /Users/chug-beacon
```

Set **no password**: the account is never logged into, and `sudo -n` needs none.
The home is `0700` on purpose — everything the daemon has to reach inside it (the
agent CLI's MCP-log cache, #490 D6) it reaches **through the escalation**, so it
needs no access of its own (`sweep_agent_cache`, `crates/container/src/host.rs`).

The home must be an **absolute path in the passwd entry** and it must be there on
disk: `lookup` (`crates/worker/src/host_users.rs`) refuses a non-absolute one and
the daemon then refuses that project's launches, while a home that is merely
missing from disk resolves fine and fails inside the task instead. The deploy
distinguishes the two (§4).

### 3.3 The daemon's login user joins each project group

The daemon creates each task directory and `chgrp`s it to the project's group —
`0770`, so the daemon can write `meta.json` and read the task's results and no
other project can reach it (#537 §7). A non-root process may only `chgrp` to a
group it belongs to, so this membership is load-bearing, not tidiness:

```sh
sudo dseditgroup -o edit -a worksalot -t user chug-beacon
```

Grant it for **every** project group and to nobody else: no project user is ever
a member of another's group.

### 3.4 The `sudoers` line

One file, all the project users, edited through `visudo` so a syntax error cannot
lock the node out of `sudo`:

```sh
sudo visudo -f /etc/sudoers.d/chuggernaut-host-users
```

```
Runas_Alias CHUG_PROJECT_USERS = chug-beacon
worksalot ALL=(CHUG_PROJECT_USERS) NOPASSWD: ALL
```

Four things about that rule:

- **The command set cannot be narrowed** and the `Runas` list is what you narrow
  instead. A task's command is whatever its job type declares, so `ALL` on the
  right is structural; the boundary is that the login user may become **exactly**
  these uids and nothing else.
- **Give the project users no rule of their own.** The escalation is one-way by
  the absence of a rule — that is what stops a task running as `chug-beacon` from
  becoming `chug-chuggernaut` or the login user (#537 §3 C1).
- **Do not add `SETENV` or widen `env_keep`.** The launch relies on `sudo`'s
  `env_reset` and hands the task's composed environment over as a `0600` file the
  wrapper sources; `-E` would need a `SETENV` grant and put the environment back
  on the daemon's side of the boundary (#537 M2), and passing it on the command
  line would publish every secret to the process table (#537 M6).
- **The filename may not contain a dot** and the include has to be live: `sudo`
  skips files in `sudoers.d` whose names hold a `.` or end in `~`, and macOS's
  `/etc/sudoers` carries the include as `@includedir` or `#includedir` depending
  on the release — check with `grep -E '^[@#]includedir' /etc/sudoers`, and
  validate the file with `sudo visudo -c -f /etc/sudoers.d/chuggernaut-host-users`.

### 3.5 Declare it, and deploy

`build-worker.sh` rewrites the node's whole run spec, so this is the same
laptop-side command that provisions any other node property — the Mini cannot ssh
a tagged worker:

```sh
WORKER_SSH=worksalot@gumbo-air-0 CHUG_WORKER_NODE=air \
  WORKER_NATS_URL=nats://100.116.243.42:4222 \
  WORKER_MODES=container,host WORKER_SLOTS=1 WORKER_SLOTS_MAX=1 \
  WORKER_HOST_PROJECTS=acme/beacon \
  WORKER_HOST_USERS=1 \
  deploy/prod/build-worker.sh
```

Pass **every** var the node should keep, not just the new one: a var you omit is
a var the node loses. `WORKER_HOST_USERS` is `1`/`0` in `WORKER_KVM`'s exact
shape, per-node overridable as `WORKER_HOST_USERS_<node>` in
`deploy/prod/chuggernaut.env` <!-- runtime --> on the Mini, and **unset stays
unset** — off, every host task keeps running as the daemon's uid and the launch
is byte-identical to what it was before #537.

With it on, the deploy asks the node — over the ssh it already has, reading only
— exactly what the daemon's own `lookup` asks, and stops with the live daemon
untouched when the answer is wrong (#537 slice 3). It creates nothing.

---

## 4. Verifying

**On the node, before you deploy.** Each of these is one of the properties above,
asked of the view that will actually run the code:

```sh
# 1. the primary group is the project's own, not staff (the D12 check)
id chug-beacon
id -gn chug-beacon                       # => chug-beacon
id -Gn worksalot | tr ' ' '\n' | grep chug-      # the daemon is in each project group

# 2. the escalation itself, in the exact shape a launch takes
sudo -n -u chug-beacon -H -- /bin/sh -c 'id -un; id -gn; echo "$HOME"'

# 3. it is one-way: a project user cannot enter another
sudo -n -u chug-beacon -H -- sudo -n -u worksalot -- id     # must FAIL

# 4. the daemon's own state is now out of reach (M3, the point of the exercise)
sudo -n -u chug-beacon -H -- cat ~worksalot/chuggernaut-worker/worker.env   # must FAIL

# 5. the task root is traversable and not listable
sudo -n -u chug-beacon -H -- /bin/sh -c 'cd /var/lib/chuggernaut/host-tasks && ls'
#   the cd succeeds (0711), the ls is denied — that is the mode working

# 6. the ordering from §1 held: the CLI is reachable from the project user
sudo -n -u chug-beacon -H -- /usr/local/lib/chuggernaut/bin/claude --version
```

Check 4 is the one that says the boundary arrived; check 6 is the one that says
you did §1 before §3.2. A failure of 6 with 4 passing is exactly the
working-looking node §1 warns about.

**From the deploy machine.** `--report` reads the node, changes nothing, and
exits non-zero when what the node runs differs from what `chuggernaut.env`
declares:

```sh
set -a; . /tmp/chug.env; set +a          # the copy fetched from the Mini
WORKER_SSH=worksalot@gumbo-air-0 CHUG_WORKER_NODE=air \
  deploy/prod/build-worker.sh --report
```

It names **counts, never projects** — a slug is a *value* of
`WORKER_HOST_PROJECTS` and the report prints no value on either side — so a
finding sends you to a run without `--report` to be told which. A deploy run that
gets through prints what it proved:

```
build-worker: WORKER_HOST_USERS=1 — every host launch on air escalates into its
own project's unix user, and this node resolves all of them: chug-beacon
```

**On the node, after the deploy.** The daemon says at boot what it bound, one
line per project:

```sh
ssh worksalot@gumbo-air-0 'grep -i "unix user" /Users/worksalot/Library/Logs/chuggernaut/worker.log'
#   host tasks for this project run as its own unix user (design #537 D1) …
ssh worksalot@gumbo-air-0 'grep WORKER_HOST_USERS /Users/worksalot/chuggernaut-worker/worker.env'
```

Quote the remote command and spell the node's paths out. `ssh` joins its
arguments into one string the **node's** shell re-parses, so an unquoted `~` is
your laptop's home and an unquoted glob is expanded — or, under `zsh`, refused —
before the command ever leaves the machine.

A project it could **not** resolve is a `warn!` naming the project and the user,
not a boot refusal — a refusal there would brick the daemon under `KeepAlive` the
moment a project was listed before its user existed, which is why the loud
failures live at the deploy and at the launch instead (#537 D5).

**The end-to-end check is a job.** Release a `mode: host` job type for the listed
project and, while it runs, look at what the launch made:

```sh
ssh worksalot@gumbo-air-0 'ls -ld /var/lib/chuggernaut/host-tasks/host-*'
#   drwxrwx---  worksalot  chug-beacon      the 0770 group-shared task directory
ssh worksalot@gumbo-air-0 'ls -l /var/lib/chuggernaut/host-tasks/host-*/task.env'
#   -rw-------  chug-beacon  chug-beacon    0600, written THROUGH the escalation
```

Then check that the tree is **gone** after the job finishes: teardown is the half
that needs the escalation most, and the one no test on this fleet could exercise
(#537 slice 2). A leak names both the path and the escalation that could not
reach it, in the daemon's log — so an empty root and a silent log is the pass.

---

## 5. Decommissioning, and the deletion asymmetry

**Creation over ssh works. Deletion of the account record does not**, and an
operator who expects symmetry will leave records behind. `sudo dscl . -delete
/Users/chug-probe` was refused on `gumbo-air-0` with `eDSPermissionError -14120`,
**even under `sudo`** (#537 §1, the measurement D9 is drawn from). **The refusal
itself is what was measured**; the cause is the usual TCC one — the
directory-services write wants console access, or Full Disk Access for the binary
driving the session — and nothing here has confirmed it, so do not build a
workaround on it. `sysadminctl -deleteUser` is not a way around it either — it
performs the same write, though nothing here has separately measured it. So treat
the account record as **durable**, and note that no part of the platform depends
on removing one.

What that leaves is a procedure that is real, in this order:

1. **Drop the project from `WORKER_HOST_PROJECTS`** and re-run the deploy
   (§3.5's command, that entry removed). Its host launches start failing at once,
   by name, and nothing new starts as that user.
2. **Remove the `sudoers` entry** for it — `sudo visudo -f
   /etc/sudoers.d/chuggernaut-host-users`, taking the name out of the
   `Runas_Alias`. **This is the actual revocation lever**: the record you cannot
   delete is inert once the daemon may no longer become it.
3. **Remove the home**, which *is* reachable over ssh: `sudo rm -rf
   /Users/chug-beacon`. That takes the caches, the simulator device set and
   whatever the project left behind with it.
4. **Leave the account record.** Do not chase it; note it instead, so the next
   operator does not read it as a live binding.

**Re-adding a project six months later meets a record that still exists**, quite
possibly with the home you deleted. Provisioning is therefore written to be
idempotent against a stale account: check `id chug-beacon` first, recreate only
what is missing (the home, its ownership and mode, §3.2's last three lines), put
the login user back in the group, and restore the `sudoers` entry. The daemon's
own resolution accepts an existing uid — it never assumes it created one.

**A user that resolves with no home directory is the shape this asymmetry
produces**, so the deploy calls it out rather than refusing: `getpwnam_r` answers,
the daemon binds the project, and the task then runs with a `HOME` that is not
there. If you see that warning and did not mean to re-add the project, step 1 is
what you missed.

Turning the binding off entirely is `WORKER_HOST_USERS=0` (or dropping the line)
and a deploy: every host task goes back to the daemon's own uid, which is what
the whole fleet does today. That is a **widening** — one project's task can read
another's credentials again — so do it as a decision, not as a rollback reflex.

---

## 6. Troubleshooting

| Symptom | What it means | What to do |
| --- | --- | --- |
| `build-worker: WORKER_HOST_USERS is on for <node>, but <ssh> has no unix user for: acme/beacon (unix user chug-beacon);` | the deploy asked the node exactly what the daemon's `lookup` asks, and refused with the live daemon untouched | create the user (§3.2), or set `WORKER_HOST_USERS_<node>=0` until the node is provisioned |
| the same refusal naming a passwd home that is **not an absolute path** | the account is there, so nothing needs creating — it is the home *field* that is wrong | `sudo dscl . -create /Users/chug-beacon NFSHomeDirectory /Users/chug-beacon` |
| `build-worker: … did not answer when asked which of the unix users its roster implies exist` | the ssh produced no sentinel: **nothing was checked**, which is not the same as a provisioned node | fix the ssh and re-run; the deploy refuses unchecked rather than passing it |
| `build-worker: WARNING: on <node> these unix users resolve but their home directories are not there` | a decommissioned account left its record behind (§5) and the daemon will bind it | recreate the home, or drop the project from the roster |
| `build-worker: WORKER_HOST_USERS='…' is neither 1 nor 0` | the daemon's own parse would refuse it as a hard config error and the supervisor would loop that refusal | use `1` or `0` |
| the deploy refuses two projects deriving one user | `a/beacon` and `b/beacon` derive `chug-beacon`, and one uid for two projects is what the binding exists to prevent | give one of them a node of its own, or run the node with the binding off |
| every host job for one project fails naming the project, the user and the node | the daemon marked that project unresolvable at boot — it refuses rather than falling back to its own uid. The message ends by naming [`worker-host-projects.md`](worker-host-projects.md), the roster page; the provisioning it is asking for is **here** | read the daemon's boot warning; create the user (§3.2) and redeploy |
| a launch fails with `could not write …/task.env as chug-beacon: sudo -n -u chug-beacon was refused` | the `sudoers` line is missing, misspelled, or names the wrong login user | §3.4, then `sudo visudo -c -f /etc/sudoers.d/chuggernaut-host-users` |
| a task dies immediately with exit status 125 | the wrapper could not read `task.env` — the file the escalation writes and the wrapper sources | check the task directory's mode and group (§4's `ls -ld`), and that §3.3's group membership is there |
| the launch cannot create its task directory after the group change | the task root is still inside the login user's home, which the project user could traverse only while it was in `staff` | move the root node-wide (§3.1) and redeploy |
| an **agent** host launch dies inside the task on a missing `claude` | §1 was done out of order: the CLI is reachable only under the login user's home | install it at `/usr/local/lib/chuggernaut/bin` ([`chug-node-adoption.md`](chug-node-adoption.md)), re-render the plist, restart the daemon |
| a host job that used docker stops reaching the daemon | expected: the socket is `0600` owned by the login user, so the uid change withholds it (#537 D11, measured as M7) | grant it node-side per [`worker-docker-grant.md`](worker-docker-grant.md), or leave it withheld |
| a task tree survives its job in the host root | a teardown could not delete and said so — the daemon's log names both the path and the escalation that failed | check the `sudoers` entry still covers that user; remove the tree by hand with root |

---

## Related

- [design #537](../../design/537-per-project-users-macos.md) — the argument:
  §2 (per project, not per task), §3 (why a `sudo` binding and not a root
  daemon), §6 (who provisions, and the asymmetry), §7 (task directories and
  every call site that escalates), D12 (the primary group and the CLI move).
- [`worker-host-projects.md`](worker-host-projects.md) — the roster this binding
  is declared over, and what single-tenancy still decides once the uid carries
  the boundary.
- [`chug-node-adoption.md`](chug-node-adoption.md) — the node-side install steps,
  including the agent CLI at `/usr/local/lib/chuggernaut/bin` that §1 depends on.
- [`worker-docker-grant.md`](worker-docker-grant.md) — the node-side grant a
  project needs once its uid no longer owns the docker socket.
- [`docs/spec.md`](../../spec.md) §3.1 — worker nodes and node-local properties.
- `deploy/prod/build-worker.sh`, `deploy/prod/env.example`,
  `crates/worker/src/host_users.rs`, `crates/container/src/host.rs` — where these
  settings are threaded through, and where the escalation is spelled.
