//! Sandbox implementations for running commands in isolated environments.
//!
//! This module provides platform-specific sandbox approaches:
//! - `overlay`: (Linux) FUSE + namespace-based sandbox with copy-on-write filesystem
//! - `ptrace`: (Linux) ptrace-based syscall interception sandbox (experimental)
//! - `sandbox_macos`: (macOS) Kernel-enforced sandbox using sandbox-exec

#[cfg(target_os = "linux")]
pub mod overlay;

#[cfg(target_os = "linux")]
pub mod ptrace;

#[cfg(target_os = "macos")]
pub mod sandbox_macos;
