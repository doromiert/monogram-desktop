{
  description = "Monogram Desktop — GTK/libadwaita interaction prototype";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      eachSystem = nixpkgs.lib.genAttrs systems;
    in {
      packages = eachSystem (system:
        let pkgs = import nixpkgs { inherit system; };
        in { default = pkgs.callPackage ./default.nix { }; });
      apps = eachSystem (system: {
        default = { type = "app"; program = "${self.packages.${system}.default}/bin/monogram-desktop"; };
      });
      devShells = eachSystem (system:
        let pkgs = import nixpkgs { inherit system; };
        in { default = pkgs.mkShell {
          inputsFrom = [ self.packages.${system}.default ];
          packages = with pkgs; [ (python3.withPackages (ps: [ ps.pygobject3 ps.pycairo ])) gtk4 libadwaita gobject-introspection xorg-server xdotool imagemagick ];
          shellHook = ''export PYTHONPATH="$PWD/src''${PYTHONPATH:+:$PYTHONPATH}"'';
        }; });
      checks = eachSystem (system: {
        prototype = self.packages.${system}.default;
      });
    };
}
