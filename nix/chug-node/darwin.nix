# chug-node, nix-darwin half. The assertion mechanism is identical to NixOS's
# (`system.build.toplevel` throws on a failed assertion), but the surface it can
# assert about is much smaller: darwin has no `virtualisation.docker`, so the
# prune, boot and live-restore hazards have no compile-time guard here.
#
# What darwin has instead is a hazard NixOS does not: dockerd runs inside a VM,
# so a bind source the VM does not share resolves *inside the VM* — silently, as
# an empty directory. Hence two assertions the NixOS module does not need
# (design #372 §1.1, §5 A7, §5 A8).
#
# The A3 probe runs as `chug.node.user`, not as root. Activation is root, but a
# mac's container runtime is user-scoped — `deploy/prod/README.md` §8 records
# that colima has no `/var/run/docker.sock` and lives at
# `~/.colima/default/docker.sock` in the login user's docker context — so a root
# probe would fail whether or not the VM is up and warn on every
# `darwin-rebuild switch`. A warning that cannot be resolved is the failure mode
# §5 rejects, so the probe crosses to the user and the message says it did.
#
# The charter, the boundary and the drain step are in `./options.nix`. Nothing
# in this repo's CI evaluates this file; `darwin-rebuild build` on the consuming
# host is the gate.
{ config, lib, ... }:
let
  cfg = config.chug.node;
  inherit (lib) mkIf mkOption types;

  declaredUser = config.users.users.${cfg.user} or { };
  userHome = if (declaredUser.home or null) != null then declaredUser.home else "/Users/${cfg.user}";

  cacheDir = toString cfg.cacheDir;
  underSharedPath = prefix:
    let shared = lib.removeSuffix "/" (toString prefix);
    in cacheDir == shared || lib.hasPrefix "${shared}/" cacheDir;

  bootAgent = cfg.darwin.dockerBootAgent;
  launchdEntries = builtins.attrNames (
    (config.launchd.user.agents or { }) // (config.launchd.agents or { }) // (config.launchd.daemons or { })
  );
in
{
  imports = [ ./options.nix ];

  options.chug.node.darwin = {
    vmSharedPaths = mkOption {
      type = types.listOf types.path;
      default = [ userHome ];
      defaultText = lib.literalExpression "[ config.users.users.\${config.chug.node.user}.home ]";
      example = [ "/Users/worksalot" "/opt/chug" ];
      description = ''
        The host prefixes this mac's docker VM shares, matching its `colima
        start --mount` flags or Docker Desktop's file-sharing list. The default
        is the user's home directory, which colima shares writable out of the
        box. A bind source outside these prefixes resolves inside the VM rather
        than on the mac.
      '';
    };

    dockerBootAgent = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "colima";
      description = ''
        The name of the `launchd` entry in this configuration that starts the
        container runtime at boot, or the literal `"external"` to record that
        boot persistence is handled outside this closure. Null warns.
      '';
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.cacheDir == null || lib.any underSharedPath cfg.darwin.vmSharedPaths;
        message = ''
          chug.node: cacheDir ${cacheDir} is not under any of
          chug.node.darwin.vmSharedPaths, so the docker VM cannot see it. The
          bind would not fail — dockerd would create an empty directory of that
          name inside the VM and bind that, leaving a cache that exists on the
          mac, is never written, and is discarded with the VM. Put it under a
          shared prefix (the user's home is shared out of the box), or declare
          the prefix this mac shares in vmSharedPaths.
        '';
      }
      {
        assertion = bootAgent == null || bootAgent == "external" || lib.elem bootAgent launchdEntries;
        message = ''
          chug.node: chug.node.darwin.dockerBootAgent names "${toString bootAgent}",
          which is not a launchd entry in this configuration. Name an entry
          under launchd.agents, launchd.daemons or launchd.user.agents, or use
          "external" to record that boot persistence lives outside this closure.
        '';
      }
    ];

    warnings = lib.optional (bootAgent == null) ''
      chug.node: nothing in this configuration declares that the container
      runtime starts at boot, so a reboot leaves the worker down with
      `--restart=always` irrelevant — there is no daemon left to honour it — and
      the node reading UNHEALTHY until someone looks. Set
      chug.node.darwin.dockerBootAgent to the launchd entry that starts it, or
      to "external" to record that it is handled outside this closure.
    '';

    system.activationScripts.postActivation.text = ''
      ${lib.optionalString (cfg.cacheDir != null) ''
        install -d -o ${cfg.user} -m 0755 ${cacheDir}
      ''}
      chug_node_as_user="/usr/bin/sudo -n -u ${cfg.user} -i"
      if [ "$(id -un)" = "${cfg.user}" ]; then
        chug_node_as_user=""
      fi
      if ! $chug_node_as_user docker info >/dev/null 2>&1; then
        echo "chug.node: docker did not answer for ${cfg.user}, so this node cannot run tasks until its container runtime is up. The probe runs as that user through a login shell because a mac's runtime is user-scoped — colima's socket is under the user's home and has no /var/run/docker.sock, so a root probe would answer the wrong question and warn on every rebuild. A negative result means the runtime is down, docker is not on that user's PATH, or this activation could not sudo without a password." >&2
      fi
    '';
  };
}
