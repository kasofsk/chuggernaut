# chug-node, NixOS half: contribute the correct value, then assert the merged
# one — an assertion reads the final `config`, so `mkForce` can only turn a
# silent breakage into a `nixos-rebuild build` failure (design #372 §5).
#
# `mkDefault` is the right priority for every contribution here, including
# `live-restore`: nixpkgs *declares* that one as a sub-option of the
# `daemon.settings` submodule with `default = versionOlder stateVersion "24.11"`
# and never defines it in its own `config`, so there is no definition for ours
# to collide with and a host still overrides it plainly, without `mkForce`.
#
# The charter, the boundary and the drain step are in `./options.nix`; read it
# before adding anything here. Nothing in this repo's CI evaluates this file
# (#372 §2.3) — the consuming host repo's `nixos-rebuild build` is the gate.
{ config, lib, ... }:
let
  cfg = config.chug.node;
  docker = config.virtualisation.docker;
  inherit (lib) mkDefault mkIf;

  managedImageFilter = "--filter=label!=chug.managed";
  labelExcludeFlags = builtins.filter (lib.hasInfix "label!=") docker.autoPrune.flags;
  sparesManagedImages = lib.any (lib.hasInfix "label!=chug.managed") docker.autoPrune.flags;
  liveRestore = docker.daemon.settings.live-restore or false;

  nodeUser = config.users.users.${cfg.user};
  cacheOwnerGroup = if nodeUser.group == "" then "-" else nodeUser.group;
in
{
  imports = [ ./options.nix ];

  config = mkIf cfg.enable {
    virtualisation.docker = {
      enable = mkDefault true;
      enableOnBoot = mkDefault true;
      daemon.settings.live-restore = mkDefault true;
      autoPrune.flags = [ managedImageFilter ];
    };

    users.users.${cfg.user}.extraGroups = [ "docker" ];

    systemd.tmpfiles.rules = lib.optional (cfg.cacheDir != null)
      "d ${toString cfg.cacheDir} 0755 ${cfg.user} ${cacheOwnerGroup} - -";

    warnings = lib.optional (docker.storageDriver != null) ''
      chug.node: virtualisation.docker.storageDriver is set on a worker node.
      Only a *change* to it is destructive, which nix cannot see, so this is a
      warning rather than an assertion: changing the storage driver makes every
      existing image and container inaccessible, and on this node that is the
      whole agent image set.
    '';

    assertions = [
      {
        assertion = docker.enable;
        message = ''
          chug.node: virtualisation.docker.enable is false. The worker daemon
          and every job container run on this node's docker socket.
        '';
      }
      {
        assertion = docker.enableOnBoot;
        message = ''
          chug.node: virtualisation.docker.enableOnBoot is false. Every
          container this platform starts uses `--restart=always`, which needs
          dockerd actually running rather than merely socket-activated, so the
          node would come back from a reboot dead and silent.
        '';
      }
      {
        assertion = liveRestore;
        message = ''
          chug.node: virtualisation.docker.daemon.settings.live-restore is not
          true, so any rebuild that touches the docker package or its settings
          restarts dockerd and kills every in-flight job container. The cost of
          the setting is that live-restore is incompatible with docker swarm —
          a host that wants swarm cannot be a chug node.
        '';
      }
      {
        assertion = lib.elem "docker" nodeUser.extraGroups;
        message = ''
          chug.node: ${cfg.user} is not in the `docker` group in the merged
          configuration. `deploy/prod/build-worker.sh` ssh's in as that user and
          runs `docker build` and `docker inspect` directly, and without the
          group every leg of that path fails as if the socket were broken.
        '';
      }
      {
        assertion = !docker.autoPrune.enable || sparesManagedImages;
        message = ''
          chug.node: virtualisation.docker.autoPrune is enabled without a
          `label!=chug.managed` exclusion in its flags. `docker system prune`
          removes images no running container is using, and the agent images
          back no running container by design, so an unfiltered nightly sweep
          deletes the node's whole image set. The exclusion must name
          `chug.managed` and must never name `chuggernaut.managed`, which is the
          container-ownership marker the dispatcher's startup sweep reaps.
        '';
      }
      {
        assertion = !docker.autoPrune.enable || builtins.length labelExcludeFlags <= 1;
        message = ''
          chug.node: virtualisation.docker.autoPrune.flags carries
          ${toString (builtins.length labelExcludeFlags)} `label!=` filters, and
          at most one is allowed. moby#40286 (open since 2019) ORs multiple
          `label!` prune filters where every other filter combination is ANDed,
          so a second exclusion means "prune anything lacking either label" and
          spares nothing while reading like belt and braces. Express the other
          exclusion as a `label` allow-filter or as its own timer.
        '';
      }
    ];
  };
}
