{
  perSystem =
    { pkgs, lib, ... }:
    let
      # Use nightly Rust toolchain (required by reverie/sandbox)
      rustToolchain = pkgs.rust-bin.selectLatestNightlyWith (
        toolchain:
        toolchain.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
        }
      );
    in
    {
      devShells.default = pkgs.mkShell {
        packages =
          with pkgs;
          [
            # Nightly Rust toolchain with all necessary components
            rustToolchain

            # Build dependencies
            pkg-config
          ]
          ++ lib.optionals stdenv.hostPlatform.isLinux [
            # FUSE development (Linux only)
            fuse3
            # libunwind for reverie sandbox on Linux
            libunwind
            # openssl for various crates
            openssl
          ]
          ++ lib.optionals stdenv.hostPlatform.isDarwin [
            # macOS frameworks
            darwin.apple_sdk.frameworks.Security
            darwin.apple_sdk.frameworks.SystemConfiguration
          ];

        # Environment setup for rust-analyzer
        RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
      };
    };
}
