{
  description = "Monogram Desktop — native GTK/libadwaita XMPP client";
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
        default = { type = "app"; program = "${self.packages.${system}.default}/bin/monogram-desktop"; meta.description = "Native Monogram Desktop XMPP client"; };
      });
      devShells = eachSystem (system:
        let pkgs = import nixpkgs { inherit system; };
        in { default = pkgs.mkShell {
          inputsFrom = [ self.packages.${system}.default ];
          MONOGRAM_MATH_HELPER = "${self.packages.${system}.default.mathRenderer}/bin/monogram-math-renderer";
          MONOGRAM_PREBUILT_LOTTIE_HELPER = "${self.packages.${system}.default.stickerRenderer}/bin/monogram-sticker-renderer";
          MONOGRAM_BWRAP = "${pkgs.bubblewrap}/bin/bwrap";
          PROTOC = "${pkgs.protobuf}/bin/protoc";
          JAVA_HOME = "${pkgs.jdk21}/lib/openjdk";
          shellHook = ''
            export XDG_DATA_DIRS="${pkgs.gtk4}/share/gsettings-schemas/${pkgs.gtk4.name}:${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}:''${NIX_GLYCIN_LOADER_PATHS}:''${XDG_DATA_DIRS:-}"
          '';
          packages = with pkgs; [ cargo rustc rustfmt clippy pkg-config protobuf sqlite libepoxy python3 jdk21 gnome-keyring gtk4 libadwaita gobject-introspection xorg-server xdotool imagemagick prosody openssl ];
        }; });
      checks = eachSystem (system: {
        prototype = self.packages.${system}.default.overrideAttrs (_: { doCheck = true; });
      });
    };
}
