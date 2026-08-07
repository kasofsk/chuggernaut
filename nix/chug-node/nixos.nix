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
# Since design #440 D2 this module also declares ONE lifecycle: the native
# worker daemon's unit, behind `chug.node.daemon.enable` and off by default. The
# charter, the boundary, the drain step and #372 §8's four reasons answered one
# by one are in `./options.nix`; read it before adding anything here. Nothing in
# this repo's CI EVALUATES this file (#372 §2.3) — the consuming host repo's
# `nixos-rebuild build` is the gate, and `./chug-worker-unit.test.sh` checks the
# unit's text against the deploy's, which is not the same thing.
{ config, options, lib, ... }:
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

  daemon = cfg.daemon;
  # Read out of the DECLARATION, never restated. `./chug-worker-unit.test.sh`
  # pins that default against `build-worker.sh`'s, so a copy here would warn on
  # every correctly-configured node the moment the deploy's path moved.
  deployEnvFile = options.chug.node.daemon.environmentFile.default;
  # The unit is the TEMPLATE, substituted — not a second rendering of it. The
  # other renderer is `deploy/prod/build-worker.sh`, and one shape is what
  # `./chug-worker-unit.test.sh` pins (design #440 slice 7). `systemd.units`
  # rather than `systemd.services` for exactly that reason: the text is the
  # artifact. A host wanting its own `serviceConfig` gets an eval conflict on
  # this unit name, which is the loud answer; a drop-in under
  # `/etc/systemd/system/chug-worker.service.d/` is the quiet one.
  unitText = builtins.replaceStrings
    [ "@NODE@" "@ENV_FILE@" "@PATH@" "@BINARY@" ]
    [ config.networking.hostName daemon.environmentFile daemon.path daemon.binary ]
    (builtins.readFile ./chug-worker.service.in);
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

    # `wantedBy` is set here as well as in the text's `[Install]` section
    # because NixOS makes the enablement symlinks itself and never runs
    # `systemctl enable` over `/etc/systemd/system` — the section alone would
    # leave a unit that exists and never starts at boot.
    systemd.units."chug-worker.service" = mkIf daemon.enable {
      enable = true;
      text = unitText;
      wantedBy = [ "multi-user.target" ];
    };

    systemd.tmpfiles.rules = lib.optional (cfg.cacheDir != null)
      "d ${toString cfg.cacheDir} 0755 ${cfg.user} ${cacheOwnerGroup} - -";

    warnings = lib.optional (daemon.enable && daemon.environmentFile != deployEnvFile) ''
      chug.node: chug.node.daemon.environmentFile is ${daemon.environmentFile},
      which is not the path deploy/prod/build-worker.sh writes by default. The
      deploy must declare WORKER_ENV_FILE_<node> as the same path or this unit
      reads a file nothing renders — design #440 D2 splits lifecycle from run
      spec, and the two halves have to name one file.
    '' ++ lib.optional (docker.storageDriver != null) ''
      chug.node: virtualisation.docker.storageDriver is set on a worker node.
      Only a *change* to it is destructive, which nix cannot see, so this is a
      warning rather than an assertion: changing the storage driver makes every
      existing image and container inaccessible, and on this node that is the
      whole agent image set.
    '';

    assertions = [
      {
        assertion = !daemon.enable
          || (lib.hasPrefix "/" daemon.binary && lib.hasPrefix "/" daemon.environmentFile);
        message = ''
          chug.node: chug.node.daemon.binary (${daemon.binary}) and
          chug.node.daemon.environmentFile (${daemon.environmentFile}) must both
          be absolute. systemd rejects a relative ExecStart or EnvironmentFile at
          load, so the node would come back from its next reboot with no worker
          daemon and a unit that never ran.
        '';
      }
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
