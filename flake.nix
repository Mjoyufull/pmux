{
  description = "pmux - Fast TUI package manager browser for multiple package managers";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    naersk.url = "github:nix-community/naersk";
    naersk.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, flake-utils, naersk }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        naersk' = pkgs.callPackage naersk {};
      in
      {
        # Default package
        packages.default = naersk'.buildPackage {
          src = ./.;
          
          # Build dependencies
          nativeBuildInputs = with pkgs; [
            pkg-config
          ];
          
          # Runtime dependencies
          buildInputs = with pkgs; [
            sqlite
          ];
          
          # Metadata
          meta = with pkgs.lib; {
            description = "Fast TUI package manager browser for multiple package managers";
            homepage = "https://github.com/Mjoyufull/pmux";
            license = licenses.mit;
            maintainers = [ ];
            platforms = platforms.linux;
          };
        };

        # Development shell
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            # Rust toolchain
            rustc
            cargo
            rustfmt
            clippy
            
            # Build dependencies
            pkg-config
            sqlite
            
            # Development tools
            git
          ];
          
          # Environment variables
          RUST_SRC_PATH = "${pkgs.rust.packages.stable.rustPlatform.rustLibSrc}";
        };

        # Apps
        apps.default = flake-utils.lib.mkApp {
          drv = self.packages.${system}.default;
        };
      }
    );
}
