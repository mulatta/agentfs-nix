#!/bin/sh
set -e

DIR="$(dirname "$0")"

"$DIR/test-init.sh"
"$DIR/test-syscalls.sh"
"$DIR/test-run-bash.sh" || true  # Requires user namespaces (may fail in CI)
"$DIR/test-mount.sh"
"$DIR/test-symlinks.sh" || true  # Requires user namespaces (may fail in CI)
