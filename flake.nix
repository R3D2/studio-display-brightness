{
  description = "studio-display-brightness — brightness for external displays on Wayland";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    let
      # DDC monitors are reached through ddcutil, which needs the i2c-dev module
      # loaded and the user in a group that may open /dev/i2c-*. The Studio
      # Display needs none of this: logind already grants the active session
      # access to its hidraw node, so no udev rule of our own is required.
      nixosModule = { config, lib, pkgs, ... }:
        let cfg = config.services.studio-display-brightness;
        in {
          options.services.studio-display-brightness = {
            enable = lib.mkEnableOption "brightness control for external displays";

            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.default;
              description = "Which build to install.";
            };

            users = lib.mkOption {
              type = lib.types.listOf lib.types.str;
              default = [ ];
              example = [ "alice" ];
              description = ''
                Users to put in the i2c group, which is what lets them talk to
                DDC monitors without root. An Apple Studio Display does not need
                this; every other monitor does.
              '';
            };
          };

          config = lib.mkIf cfg.enable {
            environment.systemPackages = [ cfg.package pkgs.ddcutil ];

            # ddcutil speaks to monitors over the graphics card's I2C buses,
            # which only exist once this module is loaded.
            boot.kernelModules = [ "i2c-dev" ];
            hardware.i2c.enable = true;

            users.groups.i2c = { };
            users.users = lib.genAttrs cfg.users (_: {
              extraGroups = [ "i2c" ];
            });
          };
        };
    in
    {
      nixosModules.default = nixosModule;
      nixosModules.studio-display-brightness = nixosModule;
    }
    // flake-utils.lib.eachDefaultSystem (system:
      let pkgs = nixpkgs.legacyPackages.${system};
      in {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "studio-display-brightness";
          version = "0.1.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [ pkgs.makeWrapper ];

          # ddcutil is how every non-Apple display is reached, and hyprctl is
          # how we learn which screen you are looking at. Both go on the PATH
          # rather than being hoped for.
          postInstall = ''
            wrapProgram $out/bin/studio-display-brightness \
              --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.ddcutil pkgs.hyprland ]}
          '';

          meta = {
            description = "Brightness for external displays on Wayland: Apple Studio Display over USB HID, the rest over DDC/CI, plus a status bar module";
            mainProgram = "studio-display-brightness";
          };
        };

        devShells.default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt pkgs.ddcutil ];
        };
      });
}
