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
      # `--version` stamp (crates/loglense-cli/build.rs): no .git in the store.
      rev = self.shortRev or self.dirtyShortRev or "dev";

      # One derivation shape for the native and the static builds.
      loglenseWith = rustPlatform: rustPlatform.buildRustPackage {
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
        env.LOGLENSE_GIT_REV = rev;
        doCheck = false;
        meta.mainProgram = "loglense";
      };

      # musl, statically linked: `ldd` reports "not a dynamic executable"
      # (D-009, NFR-01).
      static = loglenseWith pkgs.pkgsStatic.rustPlatform;
      staticAarch64 = loglenseWith pkgs.pkgsCross.aarch64-multiplatform.pkgsStatic.rustPlatform;

      # loglense-<ver>-<target>.tar.gz holding the single binary + README.
      tarball = target: drv: pkgs.runCommand "loglense-${version}-${target}.tar.gz" { } ''
        dir=loglense-${version}-${target}
        mkdir "$dir"
        cp ${drv}/bin/loglense "$dir/"
        cp ${./README.md} "$dir/README.md"
        tar --sort=name --mtime=@1 --owner=0 --group=0 --numeric-owner -czf "$out" "$dir"
      '';
    in
    {
      packages.${system} = {
        loglense = loglenseWith pkgs.rustPlatform;
        loglense-static = static;
        loglense-static-aarch64 = staticAarch64;
        tarball = tarball "x86_64-unknown-linux-musl" static;
        tarball-aarch64 = tarball "aarch64-unknown-linux-musl" staticAarch64;
        default = self.packages.${system}.loglense;
      };
    };
}
