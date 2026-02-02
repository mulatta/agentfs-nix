{
  inputs,
  self,
  ...
}:
{
  perSystem =
    {
      pkgs,
      lib,
      ...
    }:
    let

      # Nightly required for reverie (unstable features)
      rustToolchain = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default);
      craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchain;

      srcFilter =
        path: type:
        (craneLib.filterCargoSources path type)
        || (builtins.match ".*\\.md$" path != null)
        || (builtins.match ".*\\.nix$" path != null);

      cliSrc = lib.cleanSourceWith {
        src = "${self}/cli";
        filter = srcFilter;
      };

      sdkSrc = lib.cleanSourceWith {
        src = "${self}/sdk/rust";
        filter = srcFilter;
      };

      sandboxSrc = lib.cleanSourceWith {
        src = "${self}/sandbox";
        filter = srcFilter;
      };

      commonArgs = {
        strictDeps = true;

        nativeBuildInputs =
          with pkgs;
          [
            pkg-config
            rustPlatform.bindgenHook
          ]
          ++ lib.optionals stdenv.isLinux [
            fuse3
            libunwind.dev # reverie sandbox
            openssl.dev
          ];

        buildInputs =
          lib.optionals pkgs.stdenv.isLinux [
            pkgs.libunwind
            pkgs.openssl
          ]
          ++ lib.optionals pkgs.stdenv.isDarwin [
            pkgs.darwin.apple_sdk.frameworks.Security
            pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
          ];
      };

      sdkCargoArtifacts = craneLib.buildDepsOnly (
        commonArgs
        // {
          src = sdkSrc;
          pname = "agentfs-sdk-deps";
        }
      );

      agentfs-sdk = craneLib.buildPackage (
        commonArgs
        // {
          src = sdkSrc;
          cargoArtifacts = sdkCargoArtifacts;
          pname = "agentfs-sdk";

          doInstallCargoArtifacts = true;

          meta = {
            description = "AgentFS SDK for Rust";
            homepage = "https://github.com/tursodatabase/agentfs";
            license = lib.licenses.mit;
          };
        }
      );

      # Linux only — reverie requires Linux
      sandboxCargoArtifacts =
        if pkgs.stdenv.isLinux then
          craneLib.buildDepsOnly (
            commonArgs
            // {
              src = sandboxSrc;
              pname = "agentfs-sandbox-deps";
              preBuild = ''
                mkdir -p ../sdk
                cp -r ${sdkSrc} ../sdk/rust
              '';
            }
          )
        else
          null;

      agentfs-sandbox =
        if pkgs.stdenv.isLinux then
          craneLib.buildPackage (
            commonArgs
            // {
              src = sandboxSrc;
              cargoArtifacts = sandboxCargoArtifacts;
              pname = "agentfs-sandbox";

              preBuild = ''
                mkdir -p ../sdk
                cp -r ${sdkSrc} ../sdk/rust
              '';

              doInstallCargoArtifacts = true;

              meta = {
                description = "AgentFS sandbox library using reverie";
                homepage = "https://github.com/tursodatabase/agentfs";
                license = lib.licenses.mit;
                platforms = [
                  "x86_64-linux"
                  "aarch64-linux"
                ];
              };
            }
          )
        else
          null;

      cliCargoArtifacts = craneLib.buildDepsOnly (
        commonArgs
        // {
          src = cliSrc;
          pname = "agentfs-deps";
          cargoExtraArgs = lib.optionalString (!pkgs.stdenv.isLinux) "--no-default-features";
          preBuild = ''
            mkdir -p ../sdk
            cp -r ${sdkSrc} ../sdk/rust
          ''
          + lib.optionalString pkgs.stdenv.isLinux ''
            mkdir -p ../sandbox
            cp -r ${sandboxSrc}/* ../sandbox/
          '';
        }
      );

      agentfs = craneLib.buildPackage (
        commonArgs
        // {
          src = cliSrc;
          cargoArtifacts = cliCargoArtifacts;
          pname = "agentfs";

          cargoExtraArgs = lib.optionalString (!pkgs.stdenv.isLinux) "--no-default-features";

          preBuild = ''
            mkdir -p ../sdk
            cp -r ${sdkSrc} ../sdk/rust
          ''
          + lib.optionalString pkgs.stdenv.isLinux ''
            mkdir -p ../sandbox
            cp -r ${sandboxSrc}/* ../sandbox/
          '';

          meta = {
            description = "AgentFS - AI-native distributed filesystem";
            homepage = "https://github.com/tursodatabase/agentfs";
            license = lib.licenses.mit;
            mainProgram = "agentfs";
            platforms = lib.platforms.unix;
          };
        }
      );
    in
    {
      packages = {
        default = agentfs;
        inherit agentfs agentfs-sdk;
      }
      // lib.optionalAttrs (agentfs-sandbox != null) { inherit agentfs-sandbox; };
    };
}
