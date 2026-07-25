{
  description = "Lean 4 specification spike for the steins-domain value algebra (ADR-0059)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      # `nix develop` — the pinned Lean 4 from nixpkgs. This is the shell CI and
      # `cargo xtask lean-check` expect: hermetic, offline after the first
      # realization, and no toolchain download of its own. Mathlib is deliberately
      # absent (the spec uses Lean core only), so there is nothing to fetch.
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.lean4 ];
          # stderr, not stdout: `lake exe vectors` writes the vector file to stdout
          # and `cargo xtask lean-check` captures it.
          shellHook = ''
            lean --version >&2
          '';
        };

        # `nix develop .#elan` — elan-managed toolchains instead, for editing with
        # a Lean version other than the pin (elan reads ./lean-toolchain and
        # downloads it into ~/.elan). Kept separate because elan's shims shadow
        # `lean`/`lake`, so the two must never share a PATH.
        elan = pkgs.mkShell {
          packages = [ pkgs.elan ];
          shellHook = ''
            elan --version >&2
          '';
        };
      });
    };
}
