# chug-node — the conditions a Chuggernaut worker node's host must satisfy,
# declared once and implemented by `nixos.nix` and `darwin.nix` (design #372).
#
# The charter, and the lines that hold it (design #372 §3, §5, §7, §8):
#
#   * These options mean the same thing on both platforms. A fact that exists
#     on only one gets a platform-namespaced option (`chug.node.darwin.*`)
#     declared by that platform's module.
#   * `chug.node.*`, not `services.chug-node.*`: this is host preparation, not
#     a service. It owns ONE lifecycle and no more — the native worker daemon's
#     systemd unit, opt-in behind `chug.node.daemon.enable` and off by default,
#     so adopting the module still changes no running node (design #440 D2,
#     which amends this charter; the amendment is argued below).
#   * It does NOT declare the `chug-worker` CONTAINER, and nothing here changes
#     that. #372 §8 refused a unit over a container for four reasons; a unit
#     over an installed BINARY answers all four:
#       - R1, a unit would race the swapper: dissolves. Since #440 slice 6 the
#         swap installs a binary and asks the supervisor to restart. There is no
#         `docker rm -f` for a supervisor to read as a crash and no second
#         starter to collide with — the supervisor IS the starter.
#       - R2, two supervisors: dissolves. `--restart=always` is gone with the
#         container; `Restart=always` in this unit is the only one left.
#       - R3, two sources of truth for the run spec: SURVIVES, and is answered
#         by splitting lifecycle from run spec rather than denied. This module
#         declares the unit — binary path, User, Restart, the EnvironmentFile
#         PATH — and never a `WORKER_*` value. The run spec stays the
#         platform's, in the environment file `deploy/prod/build-worker.sh`
#         renders and #390's drift guard compares. A mismatch between the two
#         halves is a unit that REFUSES TO START naming the file it could not
#         load, which is the loud failure a split has to have.
#       - R4, image delivery: dissolves, and differently. A unit over a binary
#         has no tag to be missing and no pull policy to set, so #372 §8's
#         "move image delivery to a registry first" precondition is not
#         triggered — and this module still does not propose that move.
#   * Nothing here is a platform credential or the dispatcher's address, which
#     is why #372 §6's refusal of a drain hook stands unamended: the unit is
#     clock 1 (the system closure), the run spec it reads is clock 2, and §7's
#     two-projects test is satisfied — two projects on one node cannot want
#     different values for `Restart=always` or a binary path.
#   * The unit runs as root, and that answers #440's provisioning question: a
#     root daemon creates SYSTEM systemd scopes for host tasks, and the system
#     bus is a fixed socket path, so it needs neither `XDG_RUNTIME_DIR` nor
#     `loginctl enable-linger`. Those are an UNPRIVILEGED daemon's requirement,
#     and this module declares no way to run one.
#   * On darwin the daemon is a `launchd` agent in the login user's GUI domain
#     and this module does NOT declare it: `chug.node.daemon.enable` is asserted
#     false there. `deploy/prod/install-worker-launchd.sh` installs it, opt-in
#     and by hand, from a template no glob reaches.
#   * It owns MACHINE facts only. Per-project tooling — Flutter, Android SDK
#     composition, Xcode, a Rust toolchain — is clock 3 (#308 §H.6): it ships in
#     the project repo, by `git push`, and never enters this module in any form.
#     The test: if two projects on the same node could reasonably want different
#     values for it, it does not belong here — and that applies to an option's
#     value, not only to its name.
#   * Drain before you rebuild. Set the node's slots to 0, watch `occupied`
#     reach zero, rebuild, restore the count (docs/reference/runbooks/worker-capacity.md).
#     A4's `live-restore` makes the common rebuild safe to run hot; a reboot,
#     the first rebuild that adopts this module, and any rebuild that bumps a
#     node toolchain still need the drain.
#   * Nothing in Chuggernaut's own CI evaluates this module: a `nix/`-only diff
#     runs none of `.chug/tasks/ci.sh`'s stages and no agent image has `nix`
#     (#372 §2.3), and #440 slice 7 did not change that. Nix EVALUATION is
#     enforced downstream only — the consuming host repo's own
#     `nixos-rebuild build` / `darwin-rebuild build`. What CI does run is
#     `chug-worker-unit.test.sh` beside this file, which is text over text: it
#     pins the unit template against the unit `deploy/prod/build-worker.sh`
#     renders and this file's defaults against that script's, and it can say
#     nothing about whether the nix below evaluates.
{ lib, ... }:
let
  inherit (lib) mkEnableOption mkOption types;
in
{
  options.chug.node = {
    enable = mkEnableOption "Chuggernaut worker-node host preparation";

    user = mkOption {
      type = types.str;
      example = "worksalot";
      description = ''
        The login user deploys ssh in as. On NixOS this user is added to the
        `docker` group so the deploy can build images; darwin has no docker
        group, so that half of the guarantee does not exist there. It is no
        longer the daemon's own credential owner on Linux — since #440 D5 those
        live in a root-owned 0700 directory beside the environment file, and on
        darwin they stay under this user's home because the agent runs as them.
      '';
    };

    daemon = {
      enable = mkEnableOption ''
        the unit supervising this node's NATIVE worker daemon (design #440 D2).
        Off by default: adopting this module must change no running node, and
        the flip is the operator's — `deploy/prod/build-worker.sh` installs the
        binary and the environment file this unit reads. NixOS only; darwin
        asserts it false
      '';

      binary = mkOption {
        type = types.str;
        default = "/usr/local/bin/chuggernaut";
        description = ''
          The daemon binary the unit execs, `worker` appended. A string rather
          than a path: it is installed on the node by the deploy (extracted
          from the worker image, #440 D6), so a nix path literal would copy a
          nonexistent file into the store at evaluation.
        '';
      };

      environmentFile = mkOption {
        type = types.str;
        default = "/etc/chuggernaut/worker.env";
        description = ''
          The run spec's file, read by `EnvironmentFile=`. This module declares
          where it is and never what is in it — that is #372 §8's R3 answered by
          a split, and it holds only while this value and the deploy's
          `WORKER_ENV_FILE_<node>` name the same file.
        '';
      };

      path = mkOption {
        type = types.str;
        default = "/run/current-system/sw/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
        description = ''
          The `PATH` the unit gives the daemon, which shells out to git, ssh and
          docker and whose systemd default names a NixOS node's copies of none
          of them. It is also the value a host task's launch floor carries
          (#440 slice 1), so it is a machine fact rather than a run-spec knob.
        '';
      };
    };

    cacheDir = mkOption {
      type = types.nullOr types.path;
      default = null;
      example = "/var/cache/chuggernaut/sccache";
      description = ''
        The node-local build cache (`WORKER_CACHE_DIR`, spec §3.1), created here
        and owned by `chug.node.user` rather than conjured by dockerd at first
        bind. Null leaves caching off, which is always safe. On darwin this path
        must be visible inside the docker VM, which the darwin module asserts.
      '';
    };
  };
}
