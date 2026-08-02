{
  description = "Chuggernaut worker-node host preparation";

  outputs = { self }: {
    nixosModules.chug-node = import ./nix/chug-node/nixos.nix;
    darwinModules.chug-node = import ./nix/chug-node/darwin.nix;
  };
}
