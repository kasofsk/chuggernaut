#!/bin/sh
# Worker self-refresh (spec §3.1) — runs ON the worker node, invoked by the
# `chuggernaut worker` daemon when a `refresh` RPC arrives. The dispatcher host
# cannot ssh a tagged worker (Tailscale blocks tagged->tagged), so control is
# inverted: the daemon fetches the build context itself over the existing ssh
# front and rebuilds its three node images, then swaps itself.
#
# Two phases, so the daemon can quiesce launches only for the brief swap window:
#   worker-refresh.sh build <sha> <tag>   # fetch context + build 3 images
#   worker-refresh.sh swap  <tag>         # replace the daemon container
#
# All external tools (git, ssh, docker) come from PATH so the shell test can
# fake them (worker-refresh.test.sh). Config is env-driven, inherited from the
# daemon's own environment:
#   WORKER_REFRESH_GIT_URL  ssh://git@<ssh-front>:2222/<owner>/<repo>.git (required)
#   WORKER_GIT_KEY          ssh private key for the node credential (default /data/keys/worker_git)
#   WORKER_NODE, NATS_URL, NATS_CREDS   passed through to the replacement daemon
#   WORKER_CACHE_DIR        optional node-local build cache (re-applied on swap)
#   WORKER_SWAP_IMAGE       docker-cli image for the detached swapper (default docker:cli)
set -eu

PHASE="${1:?usage: worker-refresh.sh build <sha> <tag> | swap <tag>}"

case "$PHASE" in
build)
  SHA="${2:?build needs a SHA}"
  TAG="${3:?build needs a tag}"
  # Validate-first (spec §3.1): a refresh must be ATOMIC — every prerequisite is
  # checked here, BEFORE any docker mutation, so a misconfigured node fails with
  # its working images intact. The 2026-07-23 incident stranded the nuc worker
  # with NO agent images because a refresh mutated before it validated its own
  # config; nothing below this block may touch a live image until the new one is
  # built to completion.
  GIT_URL="${WORKER_REFRESH_GIT_URL:?set WORKER_REFRESH_GIT_URL (ssh://git@<ssh-front>:2222/<owner>/<repo>.git)}"
  KEY="${WORKER_GIT_KEY:-/data/keys/worker_git}"
  if [ ! -f "$KEY" ]; then
    echo "worker-refresh: git key $KEY missing — refusing build (live images untouched)" >&2
    exit 2
  fi
  export GIT_SSH_COMMAND="ssh -i $KEY -o StrictHostKeyChecking=accept-new -o IdentitiesOnly=yes"

  # Build every image under a TEMP tag first; only once all three build to
  # completion do we retag-swap them onto the live $TAG (below). A build that
  # dies part-way therefore leaves each live tag exactly as it was — a node that
  # had working images keeps them. The temp tags are dropped on exit; after a
  # successful retag they share an image id with the live tag, so `rmi` only
  # untags the temp name and the live image survives.
  NEW="$TAG-refresh"
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"; docker rmi -f "chuggernaut/worker:$NEW" "chuggernaut/agent:$NEW" "chuggernaut/agent-rust:$NEW" >/dev/null 2>&1 || true' EXIT
  # Fetch the repo's advertised HEAD (its default branch tip) over the ssh
  # front, then verify it resolves to the requested SHA before building.
  #
  # We fetch a REF, not a raw commit: agents clone by ref (container::bootstrap_cmd
  # runs `git clone --single-branch --branch "$JOB_BRANCH"`), and the ssh front's
  # bare repos only enable `uploadpack.allowFilter` (crates/vcs `ensure_upload_filter`)
  # — they do NOT set `allowAnySHA1InWant`, so a `want <raw sha>` fetch is refused
  # ("Server does not allow request for unadvertised object"). A ref fetch needs
  # no such server capability, so this rides the proven agent-clone path.
  #
  # HEAD is the requested SHA in the deploy path: prod requests refresh to the
  # commit it just checked out, which is the tip of `main` and — because the
  # platform hosts its own repo (§3.1) — exactly the ssh front's HEAD. If HEAD
  # has moved off the requested SHA we refuse rather than build the wrong tree:
  # the daemon aborts, the old images stay, and the deploy surfaces a WARNING
  # with the drift (spec §3.1) instead of silently shipping the wrong SHA.
  git -C "$TMP" init -q
  git -C "$TMP" fetch -q --depth 1 "$GIT_URL" HEAD
  GOT="$(git -C "$TMP" rev-parse FETCH_HEAD)"
  if [ "$GOT" != "$SHA" ]; then
    echo "worker-refresh: remote HEAD $GOT != requested $SHA — refusing build (drift stays, deploy warns)" >&2
    exit 1
  fi

  # Build the three images to their TEMP tags. Any failure here aborts under
  # `set -e` with the live tags untouched (the temp leftovers are pruned by the
  # trap on exit).
  #
  # Worker daemon image (repo-root context; bakes chuggernaut + channel binary
  # at this SHA — native on the node).
  git -C "$TMP" archive --format=tar FETCH_HEAD \
    | docker build -q -t "chuggernaut/worker:$NEW" \
        -f deploy/prod/Dockerfile.worker --build-arg "CHUG_GIT_SHA=$SHA" -

  # Agent images the job types run in.
  git -C "$TMP" archive --format=tar FETCH_HEAD:deploy/dev \
    | docker build -q -t "chuggernaut/agent:$NEW" -f Dockerfile.agent -
  git -C "$TMP" archive --format=tar FETCH_HEAD \
    | docker build -q -t "chuggernaut/agent-rust:$NEW" \
        -f deploy/prod/Dockerfile.agent-rust -

  # All three built to completion — retag-swap onto the live tag. `docker tag`
  # is local and instant, so the live images flip to the new build only now,
  # and only after we know every image is buildable.
  docker tag "chuggernaut/worker:$NEW"     "chuggernaut/worker:$TAG"
  docker tag "chuggernaut/agent:$NEW"      "chuggernaut/agent:$TAG"
  docker tag "chuggernaut/agent-rust:$NEW" "chuggernaut/agent-rust:$TAG"

  echo "worker-refresh: built chuggernaut/{worker,agent,agent-rust}:$TAG ($SHA)"
  ;;

swap)
  TAG="${2:?swap needs a tag}"
  # The daemon runs INSIDE chug-worker, so it cannot `docker rm -f chug-worker`
  # itself — that would kill this process mid-swap. Instead launch a DETACHED
  # sibling container (its own lifecycle) that removes the old daemon and starts
  # the new one. Job containers are untouched: `docker rm -f` only hits
  # chug-worker, and the dispatcher's poll-based wait re-attaches (spec §3.1).
  # The replacement carries the SAME env the daemon runs with (inherited here).
  NODE="${WORKER_NODE:?WORKER_NODE must be set}"
  NATS="${NATS_URL:?NATS_URL must be set}"
  CREDS="${NATS_CREDS:-/data/keys/worker.creds}"
  SWAP_IMAGE="${WORKER_SWAP_IMAGE:-docker:cli}"

  # Optional node-local build cache, re-applied so caching survives the swap.
  # ENV ONLY, exactly like build-worker.sh's daemon run: the daemon adds the
  # cache bind to each sibling job container via the docker socket (host path),
  # so the DAEMON container needs no cache mount of its own. Carrying this
  # forward is what stops a refresh from silently dropping caching (#55/#82).
  CACHE_ARGS=""
  if [ -n "${WORKER_CACHE_DIR:-}" ]; then
    CACHE_ARGS="-e WORKER_CACHE_DIR=$WORKER_CACHE_DIR"
  fi

  # Recover the REAL host bind sources from the running daemon rather than
  # reconstructing them from $HOME. build-worker.sh mounts the keys from the
  # node login user's `\$HOME/chuggernaut-worker/keys` (expanded in the node's
  # ssh shell), but the swap runs `RUN_NEW` inside the detached `docker:cli`
  # swapper where HOME=/root — re-deriving $HOME there would bind an empty
  # `/root/chuggernaut-worker/keys` and strand the new daemon without NATS creds.
  # This phase runs inside chug-worker (docker.sock mounted), so inspect the
  # live container for the literal Source paths and bake them into RUN_NEW.
  KEYS_SRC="$(docker inspect chug-worker \
    --format '{{range .Mounts}}{{if eq .Destination "/data/keys"}}{{.Source}}{{end}}{{end}}')"
  SOCK_SRC="$(docker inspect chug-worker \
    --format '{{range .Mounts}}{{if eq .Destination "/var/run/docker.sock"}}{{.Source}}{{end}}{{end}}')"
  if [ -z "$KEYS_SRC" ]; then
    echo "worker-refresh: no /data/keys mount on chug-worker; refusing swap (would strand creds)" >&2
    exit 1
  fi
  SOCK_SRC="${SOCK_SRC:-/var/run/docker.sock}"

  # The refreshed worker image already contains the (new SHA's) worker-refresh.sh
  # and daemon binary, so no script is mounted from the host. All bind sources
  # below are literal host paths (already expanded), so evaluating RUN_NEW inside
  # the swapper reproduces build-worker.sh's exact mounts.
  RUN_NEW="docker run -d --restart=always --name chug-worker \
    -v $SOCK_SRC:/var/run/docker.sock \
    -v $KEYS_SRC:/data/keys:ro \
    -e WORKER_NODE=$NODE -e NATS_URL=$NATS -e NATS_CREDS=$CREDS \
    -e WORKER_REFRESH_GIT_URL=${WORKER_REFRESH_GIT_URL:-} \
    -e WORKER_GIT_KEY=${WORKER_GIT_KEY:-/data/keys/worker_git} \
    $CACHE_ARGS chuggernaut/worker:$TAG"

  # sleep briefly so this RPC's reply flushes before the old daemon is removed.
  docker run -d --rm \
    -v /var/run/docker.sock:/var/run/docker.sock \
    "$SWAP_IMAGE" \
    sh -c "sleep 2; docker rm -f chug-worker >/dev/null 2>&1 || true; $RUN_NEW"

  echo "worker-refresh: swap scheduled -> chuggernaut/worker:$TAG on $NODE"
  ;;

*)
  echo "worker-refresh: unknown phase '$PHASE' (want build|swap)" >&2
  exit 2
  ;;
esac
