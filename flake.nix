{
  description = "Padbridge development environment";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs = { nixpkgs, rust-overlay, ... }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; overlays = [ rust-overlay.overlays.default ]; };
    in {
      devShells.${system}.default = pkgs.mkShell {
        packages = [ (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml) pkgs.cmake pkgs.pkg-config ];
        buildInputs = [ pkgs.udev pkgs.libxkbcommon ];
      };
    };
}
