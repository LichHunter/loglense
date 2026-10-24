{
  description = "loglense — time-style background command wrapper with agent-facing log lenses";

  # packages only. The dev shell is devenv (devenv.nix, via direnv); the
  # nixpkgs pin matches the ephor workspace: one closure cache.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/55bfb4d7437bd4e09ab7cba58a43bd68eb310044";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      lib = pkgs.lib;
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
    in
    {
      packages.${system} = {
        loglense = pkgs.rustPlatform.buildRustPackage {
          pname = "loglense";
          inherit version;
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./crates
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "-p" "loglense-cli" ];
          doCheck = false;
          meta.mainProgram = "loglense";
        };
        default = self.packages.${system}.loglense;
      };
    };
}
