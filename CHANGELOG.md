# Changelog

## [0.2.1] - 2025-12-04

### Fixed

- Fix `_Unwind_RaiseException` symbol lookup error on Fedora by linking to `libgcc_s.so` dynamically.
- Eliminate dependency to libfuse by using the `fuser` crate pure Rust FUSE implementation.

## [0.2.0] - 2025-12-04

### Added

- AgentFS FUSE module for mounting agent filesystems.
- TypeScript SDK: Support for custom agent filesystem path.

### Changed

- Switch to fixed-size chunks in AgentFS specification.
- TypeScript SDK: Switch to fixed-size inode chunks.
- Rust SDK: Switch to fixed-size inode chunks.
- Switch AgentFS SDK to use identifier-based API.

## [0.1.2] - 2025-11-14

### Added

- Enable Darwin/x86-64 builds for the CLI.

## [0.1.1] - 2025-11-14

### Added

- Example using OpenAI Agents SDK and AgentFS.
- Example using Claude Agent SDK and AgentFS.

### Fixed

- CLI `ls` command now recursively lists all files.

## [0.1.0] - 2025-11-13

### Added

- Initial release of AgentFS CLI.
- TypeScript SDK with async factory method (`AgentFS.open()`).
- Sandbox command for running agents in isolated environments.
- Passthrough VFS for transparent filesystem access.
- Symlink syscall support in sandbox.
- Cross-platform builds (Linux, macOS).
- Example agent implementations.

[Unreleased]: https://github.com/tursodatabase/agentfs/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/tursodatabase/agentfs/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/tursodatabase/agentfs/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/tursodatabase/agentfs/releases/tag/v0.1.0
