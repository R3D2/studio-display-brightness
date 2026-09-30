{
  description = "studio-display-brightness — Apple Studio Display and DDC/CI monitor brightness for Linux";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    let
      # Apple's vendor id, and the displays that carry this brightness control:
      # the Studio Display and the two Pro Display XDR variants.
      appleDisplayIds = [ "1114" "1116" "1118" ];

      nixosModule = { config, lib, pkgs, ... }:
        let
          cfg = config.services.studio-display-brightness;
          rule = id:
            ''SUBSYSTEM=="hidraw", KERNEL=="hidraw*", ATTRS{idVendor}=="05ac", ''
            + ''ATTRS{idProduct}=="${id}", MODE="0660", TAG+="uaccess"'';
        in
        {
          options.services.studio-display-brightness = {
            enable = lib.mkEnableOption "brightness control for external displays";

            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.default;
              description = "Which build to install.";
            };

            ddc = lib.mkOption {
              type = lib.types.bool;
              default = true;
              description = ''
                Support monitors other than Apple displays, which speak DDC/CI
                over the graphics card's I2C buses. Turn it off if the only
                display you care about is a Studio Display: it saves installing
                ddcutil and opening the I2C buses to a group.
              '';
            };

            users = lib.mkOption {
              type = lib.types.listOf lib.types.str;
              default = [ ];
              example = [ "alice" ];
              description = ''
                Users to put in the `i2c` group, which is what lets them reach
                DDC monitors without root. Apple displays need nothing here:
                they are reached over USB HID, and the udev rule this module
                installs hands the active session an ACL on the device.
              '';
            };
          };

          config = lib.mkIf cfg.enable (lib.mkMerge [
            {
              environment.systemPackages = [ cfg.package ];

              # Without this the hidraw nodes are root-only and the Studio
              # Display cannot be reached at all. `TAG+="uaccess"` is what makes
              # logind grant the logged-in session an ACL on them, so no group
              # membership and no root is needed — but the tag has to come from
              # somewhere, and nothing in a default install provides it for
              # hidraw.
              services.udev.extraRules =
                lib.concatMapStringsSep "\n" rule appleDisplayIds;
            }

            (lib.mkIf cfg.ddc {
              environment.systemPackages = [ pkgs.ddcutil ];

              # Loads i2c-dev and creates the i2c group; the udev rules that put
              # /dev/i2c-* in it come with it.
              hardware.i2c.enable = true;

              users.users = lib.genAttrs cfg.users (_: {
                extraGroups = [ "i2c" ];
              });
            })
          ]);
        };

      homeModule = { config, lib, pkgs, ... }:
        let cfg = config.programs.studio-display-brightness;
        in {
          options.programs.studio-display-brightness = {
            enable = lib.mkEnableOption "brightness control for external displays";

            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.default;
              description = "Which build to install.";
            };

            wayle = {
              enable = lib.mkEnableOption ''
                the wayle status bar module. This defines the module; placing it
                is still yours to do, by adding "custom-<id>" to a bar layout —
                where in the bar it belongs is not something this can guess
              '';

              id = lib.mkOption {
                type = lib.types.str;
                default = "studio-display-brightness";
                description = ''
                  The module id. A bar layout refers to it as `custom-<id>`.
                '';
              };

              step = lib.mkOption {
                type = lib.types.ints.between 1 50;
                default = 5;
                description = "How far one notch of the scroll wheel moves it.";
              };

              icons = lib.mkOption {
                type = lib.types.attrsOf lib.types.str;
                default = { };
                example = {
                  dim = "ri-contrast-line-symbolic";
                  mid = "ri-sun-line-symbolic";
                  bright = "ri-sun-fill-symbolic";
                };
                description = ''
                  Icon per level, keyed by the class `watch` reports: `dim`
                  below 20%, `bright` above 80%, `mid` in between. Empty by
                  default, because icon names are whichever theme you have
                  installed and a wrong one renders as nothing at all.
                '';
              };

              extraSettings = lib.mkOption {
                type = lib.types.attrs;
                default = { };
                example = { label-max-length = 22; border-show = true; };
                description = ''
                  Anything else the bar takes for a custom module — colours,
                  borders, label length. Merged over what is set here, so it can
                  also override it.
                '';
              };
            };
          };

          config = lib.mkIf cfg.enable (lib.mkMerge [
            { home.packages = [ cfg.package ]; }

            (lib.mkIf cfg.wayle.enable (
              let bin = "${cfg.package}/bin/studio-display-brightness";
              in {
                services.wayle.settings.modules.custom = lib.mkAfter [
                  ({
                    id = cfg.wayle.id;
                    command = "${bin} watch";
                    mode = "watch";
                    restart-policy = "on-exit";
                    format = "{{ text }}";
                    tooltip-format = "{{ tooltip }}";
                    class-format = "{{ class }}";

                    # A custom module is not told which bar it is drawn on, so
                    # it follows the focused screen. With input:follow_mouse = 1
                    # that is the screen under the pointer, which is the bar
                    # being scrolled.
                    scroll-up = "${bin} up --step ${toString cfg.wayle.step}";
                    scroll-down = "${bin} down --step ${toString cfg.wayle.step}";
                    left-click = "${bin} cycle";
                    right-click = "${bin} set 100";
                  }
                  // lib.optionalAttrs (cfg.wayle.icons != { }) {
                    icon-map = cfg.wayle.icons;
                  }
                  // cfg.wayle.extraSettings)
                ];
              }
            ))
          ]);
        };
    in
    {
      nixosModules.default = nixosModule;
      nixosModules.studio-display-brightness = nixosModule;
      # Both spellings: `homeModules` is what home-manager settled on, and
      # `homeManagerModules` is what a good deal of existing config still says.
      homeModules.default = homeModule;
      homeModules.studio-display-brightness = homeModule;
      homeManagerModules.default = homeModule;
      homeManagerModules.studio-display-brightness = homeModule;

      overlays.default = final: _prev: {
        studio-display-brightness = self.packages.${final.system}.default;
      };
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

          # ddcutil reaches every non-Apple monitor, hyprctl says which screen
          # you are looking at, and notify-send is the OSD that `--notify` asks
          # for. All three go on the PATH rather than being hoped for.
          postInstall = ''
            wrapProgram $out/bin/studio-display-brightness \
              --prefix PATH : ${pkgs.lib.makeBinPath [
                pkgs.ddcutil
                pkgs.hyprland
                pkgs.libnotify
              ]}
          '';

          meta = {
            description = "Apple Studio Display and DDC/CI monitor brightness for Linux, per monitor, with a status bar module";
            mainProgram = "studio-display-brightness";
            license = pkgs.lib.licenses.mit;
            platforms = pkgs.lib.platforms.linux;
          };
        };

        devShells.default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt pkgs.ddcutil ];
        };
      });
}
