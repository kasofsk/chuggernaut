# chug-node — the conditions a Chuggernaut worker node's host must satisfy,
# declared once and implemented by `nixos.nix` and `darwin.nix` (design #372).
#
# The charter, and the lines that hold it (design #372 §3, §5, §7, §8):
#
#   * These options mean the same thing on both platforms. A fact that exists
#     on only one gets a platform-namespaced option (`chug.node.darwin.*`)
#     declared by that platform's module.
#   * `chug.node.*`, not `services.chug-node.*`: this is host preparation, not
#     a service. It owns no lifecycle.
#   * It does NOT declare the `chug-worker` container or any unit supervising
#     it. `deploy/prod/worker-refresh.sh` swaps that container by `docker rm -f`
#     plus `docker run -d --restart=always`, which a supervising unit would read
#     as a crash and race (#372 §8).
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
#     (#372 §2.3). Its only enforcement today is downstream — the consuming host
#     repo's own `nixos-rebuild build` / `darwin-rebuild build`.
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
        The login user deploys ssh in as and the worker daemon's keys live
        under. On NixOS this user is added to the `docker` group; darwin has no
        docker group, so that half of the guarantee does not exist there. On
        both platforms `$HOME/chuggernaut-worker/keys` is the bind source for
        the daemon's `/data/keys`.
      '';
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
