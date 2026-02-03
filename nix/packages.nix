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
      hashes = builtins.fromJSON (builtins.readFile ./hashes.json);

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

      # preBuild snippets for crates with path dependencies
      sdkPathDeps = ''
        mkdir -p ../sdk
        cp -r ${sdkSrc} ../sdk/rust
      '';

      allPathDeps = ''
        mkdir -p ../sdk
        cp -r ${sdkSrc} ../sdk/rust
        mkdir -p ../sandbox
        cp -r ${sandboxSrc}/* ../sandbox/
      '';

      commonArgs = {
        strictDeps = true;

        nativeBuildInputs =
          with pkgs;
          [
            pkg-config
            rustPlatform.bindgenHook
          ]
          ++ lib.optionals stdenv.hostPlatform.isLinux [
            fuse3
            libunwind.dev # reverie sandbox
            openssl.dev
          ];

        buildInputs =
          with pkgs;
          lib.optionals stdenv.hostPlatform.isLinux [
            libunwind
            openssl
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
        if pkgs.stdenv.hostPlatform.isLinux then
          craneLib.buildDepsOnly (
            commonArgs
            // {
              src = sandboxSrc;
              pname = "agentfs-sandbox-deps";
              preBuild = sdkPathDeps;
            }
          )
        else
          null;

      agentfs-sandbox =
        if pkgs.stdenv.hostPlatform.isLinux then
          craneLib.buildPackage (
            commonArgs
            // {
              src = sandboxSrc;
              cargoArtifacts = sandboxCargoArtifacts;
              pname = "agentfs-sandbox";

              preBuild = sdkPathDeps;

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
          cargoExtraArgs = lib.optionalString (!pkgs.stdenv.hostPlatform.isLinux) "--no-default-features";
          # Cargo resolves all path deps even with --no-default-features
          preBuild = allPathDeps;
        }
      );

      agentfs = craneLib.buildPackage (
        commonArgs
        // {
          src = cliSrc;
          cargoArtifacts = cliCargoArtifacts;
          pname = "agentfs";

          cargoExtraArgs = lib.optionalString (!pkgs.stdenv.hostPlatform.isLinux) "--no-default-features";

          preBuild = allPathDeps;

          meta = {
            description = "AgentFS - AI-native distributed filesystem";
            homepage = "https://github.com/tursodatabase/agentfs";
            license = lib.licenses.mit;
            mainProgram = "agentfs";
            platforms = lib.platforms.unix;
          };
        }
      );

      pyturso = pkgs.python3Packages.buildPythonPackage rec {
        pname = "pyturso";
        inherit (hashes.pyturso) version;
        pyproject = true;

        src = pkgs.fetchPypi {
          inherit pname version;
          inherit (hashes.pyturso) hash;
        };

        cargoDeps = pkgs.rustPlatform.importCargoLock {
          lockFile = ./pyturso-Cargo.lock;
          outputHashes = hashes.pyturso.cargoOutputHashes;
        };

        build-system = with pkgs; [
          maturin
          rustPlatform.cargoSetupHook
          rustPlatform.maturinBuildHook
        ];

        dependencies = with pkgs.python3Packages; [ typing-extensions ];

        doCheck = false; # requires database

        meta = {
          description = "Python binding for Turso database client";
          homepage = "https://github.com/tursodatabase/pyturso";
          license = lib.licenses.mit;
        };
      };

      agentfs-sdk-python = pkgs.python3Packages.buildPythonPackage rec {
        pname = "agentfs-sdk";
        version = "0.6.0-pre.4";
        pyproject = true;

        src = lib.cleanSource "${self}/sdk/python";

        build-system = with pkgs.python3Packages; [
          setuptools
        ];

        dependencies = [ pyturso ];

        doCheck = false; # requires agentfs server

        meta = {
          description = "AgentFS Python SDK - A filesystem and key-value store for AI agents";
          homepage = "https://github.com/tursodatabase/agentfs";
          license = lib.licenses.mit;
        };
      };

      agentfs-sdk-typescript = pkgs.buildNpmPackage rec {
        pname = "agentfs-sdk";
        version = "0.6.0-pre.4";

        src = lib.cleanSource "${self}/sdk/typescript";

        inherit (hashes.typescriptSdk) npmDepsHash;

        buildPhase = ''
          runHook preBuild
          npm run build
          runHook postBuild
        '';

        installPhase = ''
          runHook preInstall
          mkdir -p $out/lib/node_modules/${pname}
          cp -r dist $out/lib/node_modules/${pname}/
          cp package.json $out/lib/node_modules/${pname}/
          runHook postInstall
        '';

        meta = {
          description = "AgentFS TypeScript SDK";
          homepage = "https://github.com/tursodatabase/agentfs";
          license = lib.licenses.mit;
        };
      };

      packages = {
        inherit
          agentfs
          agentfs-sdk
          agentfs-sdk-python
          agentfs-sdk-typescript
          ;
      }
      // lib.optionalAttrs (agentfs-sandbox != null) { inherit agentfs-sandbox; };
    in
    {
      packages = packages // {
        default = agentfs;
      };
      checks = lib.mapAttrs' (name: drv: lib.nameValuePair "pkgs-${name}" drv) packages;
    };
}
