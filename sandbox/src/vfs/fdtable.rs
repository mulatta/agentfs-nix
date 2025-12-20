use super::file::BoxedFileOps;
use std::collections::{BinaryHeap, HashMap};
use std::sync::{Arc, Mutex};

/// Standard file descriptor constants
const STDIN_FILENO: i32 = 0;
const STDOUT_FILENO: i32 = 1;
const STDERR_FILENO: i32 = 2;
const FIRST_USER_FD: i32 = 3;

/// Information about a virtualized file descriptor
#[derive(Clone)]
pub enum FdEntry {
    /// Passthrough file - just maps virtual FD to kernel FD
    Passthrough {
        kernel_fd: i32,
        flags: i32,
        path: Option<std::path::PathBuf>,
    },
    /// Virtual file - has FileOps implementation
    Virtual {
        file_ops: BoxedFileOps,
        flags: i32,
        path: Option<std::path::PathBuf>,
    },
}

impl FdEntry {
    /// Get the kernel file descriptor if this is a passthrough file
    pub fn kernel_fd(&self) -> Option<i32> {
        match self {
            FdEntry::Passthrough { kernel_fd, .. } => Some(*kernel_fd),
            FdEntry::Virtual { .. } => None,
        }
    }

    /// Get the flags for this FD entry
    pub fn flags(&self) -> i32 {
        match self {
            FdEntry::Passthrough { flags, .. } => *flags,
            FdEntry::Virtual { flags, .. } => *flags,
        }
    }

    /// Get the path for this FD entry
    pub fn path(&self) -> Option<&std::path::PathBuf> {
        match self {
            FdEntry::Passthrough { path, .. } => path.as_ref(),
            FdEntry::Virtual { path, .. } => path.as_ref(),
        }
    }

    /// Get the file_ops for virtual files
    pub fn file_ops(&self) -> Option<&BoxedFileOps> {
        match self {
            FdEntry::Passthrough { .. } => None,
            FdEntry::Virtual { file_ops, .. } => Some(file_ops),
        }
    }
}

/// Inner state of the FD table, protected by a single mutex
struct FdTableInner {
    /// Mapping from virtual FD to kernel FD
    entries: HashMap<i32, FdEntry>,
    /// Next virtual FD to allocate (monotonically increasing)
    next_vfd: i32,
    /// Min-heap of freed FDs available for reuse (stored as negative for min-heap behavior)
    free_fds: BinaryHeap<std::cmp::Reverse<i32>>,
}

/// Per-process file descriptor table that virtualizes file descriptors
///
/// This table maintains a mapping from virtual (process-visible) file descriptors
/// to kernel (actual) file descriptors. It is thread-safe and can be shared across
/// threads within the same process.
///
/// Note: Clone creates a shallow copy that shares the same underlying FD table.
/// For fork/clone syscalls, use `deep_clone()` instead.
#[derive(Clone)]
pub struct FdTable {
    inner: Arc<Mutex<FdTableInner>>,
}

impl FdTable {
    /// Create a new FD table with standard FDs (stdin, stdout, stderr)
    pub fn new() -> Self {
        let mut entries = HashMap::new();

        // Initialize standard file descriptors (0, 1, 2) as passthrough files
        entries.insert(
            STDIN_FILENO,
            FdEntry::Passthrough {
                kernel_fd: STDIN_FILENO,
                flags: 0,
                path: None,
            },
        );
        entries.insert(
            STDOUT_FILENO,
            FdEntry::Passthrough {
                kernel_fd: STDOUT_FILENO,
                flags: 0,
                path: None,
            },
        );
        entries.insert(
            STDERR_FILENO,
            FdEntry::Passthrough {
                kernel_fd: STDERR_FILENO,
                flags: 0,
                path: None,
            },
        );

        Self {
            inner: Arc::new(Mutex::new(FdTableInner {
                entries,
                next_vfd: FIRST_USER_FD,
                free_fds: BinaryHeap::new(),
            })),
        }
    }

    /// Create a deep clone of this FD table (for fork/clone syscalls)
    ///
    /// This creates a completely independent copy of the FD table,
    /// unlike the default Clone which shares the underlying table.
    pub fn deep_clone(&self) -> Self {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        Self {
            inner: Arc::new(Mutex::new(FdTableInner {
                entries: inner.entries.clone(),
                next_vfd: inner.next_vfd,
                free_fds: inner.free_fds.clone(),
            })),
        }
    }

    /// Allocate a new virtual FD for the given FdEntry
    ///
    /// This uses the lowest available FD number, as required by POSIX.
    pub fn allocate(&self, entry: FdEntry) -> i32 {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Try to reuse a freed FD first (POSIX requires lowest available FD)
        let vfd = if let Some(std::cmp::Reverse(free_fd)) = inner.free_fds.pop() {
            free_fd
        } else {
            // No free FDs, allocate a new one
            let vfd = inner.next_vfd;
            if vfd == i32::MAX {
                // FD exhaustion - search for gaps in allocated FDs
                // This is a rare edge case
                (FIRST_USER_FD..i32::MAX)
                    .find(|fd| !inner.entries.contains_key(fd))
                    .expect("File descriptor table exhausted")
            } else {
                inner.next_vfd += 1;
                vfd
            }
        };

        inner.entries.insert(vfd, entry);
        vfd
    }

    /// Allocate a new virtual FD at or above the specified minimum
    ///
    /// This is used for fcntl F_DUPFD and F_DUPFD_CLOEXEC commands.
    pub fn allocate_min(&self, min_vfd: i32, entry: FdEntry) -> i32 {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Find the lowest available FD >= min_vfd
        let vfd = (min_vfd..i32::MAX)
            .find(|fd| !inner.entries.contains_key(fd))
            .expect("File descriptor table exhausted");

        // Update next_vfd if we allocated beyond it
        if vfd >= inner.next_vfd {
            // // We just "skipped over" the range [next_vfd, vfd). Those FDs are now
            // // valid, unused, and must be eligible for `allocate()` (lowest-available).
            // let mut fd = inner.next_vfd;
            // while fd < vfd {
            //     if fd >= FIRST_USER_FD && !inner.entries.contains_key(&fd) {
            //         inner.free_fds.push(std::cmp::Reverse(fd));
            //     }
            //     fd = fd.checked_add(1).expect("fd overflow");
            // }
            inner.next_vfd = vfd + 1;
        }

        // Remove from free list if it was there
        inner.free_fds = inner
            .free_fds
            .clone()
            .into_iter()
            .filter(|&std::cmp::Reverse(fd)| fd != vfd)
            .collect();

        inner.entries.insert(vfd, entry);
        vfd
    }

    /// Allocate a specific virtual FD (used for dup2)
    ///
    /// Returns the old FdEntry if the VFD was already allocated, which the caller
    /// should close if needed.
    pub fn allocate_at(&self, vfd: i32, entry: FdEntry) -> Option<FdEntry> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Remove the FD from free list if it's there
        // (This is inefficient but dup2 to freed FDs is rare)
        inner.free_fds = inner
            .free_fds
            .clone()
            .into_iter()
            .filter(|&std::cmp::Reverse(fd)| fd != vfd)
            .collect();

        // Update next_vfd if necessary
        if vfd >= inner.next_vfd {
            // // Like `allocate_min`, allocating at a far FD creates gaps that must remain
            // // available for subsequent `allocate()` calls.
            // let mut fd = inner.next_vfd;
            // while fd < vfd {
            //     if fd >= FIRST_USER_FD && !inner.entries.contains_key(&fd) {
            //         inner.free_fds.push(std::cmp::Reverse(fd));
            //     }
            //     fd = fd.checked_add(1).expect("fd overflow");
            // }
            inner.next_vfd = vfd + 1;
        }

        // Insert the new entry and return the old one if it existed
        inner.entries.insert(vfd, entry)
    }

    /// Translate a virtual FD to a kernel FD
    ///
    /// Returns the kernel FD if this is a passthrough file, or None if it's a
    /// virtualized file or the VFD doesn't exist.
    pub fn translate(&self, vfd: i32) -> Option<i32> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.entries.get(&vfd).and_then(|entry| entry.kernel_fd())
    }

    /// Get the full entry for a virtual FD
    pub fn get(&self, vfd: i32) -> Option<FdEntry> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.entries.get(&vfd).cloned()
    }

    /// Deallocate a virtual FD and mark it as available for reuse
    pub fn deallocate(&self, vfd: i32) -> Option<FdEntry> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let entry = inner.entries.remove(&vfd)?;

        // Add to free list for reuse (unless it's a standard FD)
        if vfd >= FIRST_USER_FD {
            inner.free_fds.push(std::cmp::Reverse(vfd));
        }

        Some(entry)
    }

    /// Duplicate a virtual FD (for dup syscall)
    pub fn duplicate(&self, old_vfd: i32) -> Option<i32> {
        let entry = self.get(old_vfd)?;
        // Allocate a new virtual FD pointing to the same file operations
        Some(self.allocate(entry))
    }

    /// Duplicate a virtual FD to a specific new FD (for dup2 syscall)
    ///
    /// Returns the old entry that was at new_vfd if it existed (caller should close it)
    pub fn duplicate_at(&self, old_vfd: i32, new_vfd: i32) -> Option<FdEntry> {
        let entry = self.get(old_vfd)?;
        self.allocate_at(new_vfd, entry)
    }
}

impl Default for FdTable {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for FdTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock().unwrap();
        f.debug_struct("FdTable")
            .field("entry_count", &inner.entries.len())
            .field("next_vfd", &inner.next_vfd)
            .field("free_fds_count", &inner.free_fds.len())
            .finish()
    }
}

/// Replay helpers for reproducing FD-table failures without running proptest.
///
/// The proptest suite can emit a JSON artifact containing a `ReproCase`. Anyone can then run:
/// - `cargo run -p agentfs-sandbox --bin fdtable_replay -- <path-to-case.json>`
pub mod repro {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ReproCase {
        pub ops: Vec<ReproOp>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub enum ReproOp {
        DeepClone,
        Allocate { kernel_fd: i32 },
        AllocateMin { min_fd: i32, kernel_fd: i32 },
        AllocateAt { vfd: i32, kernel_fd: i32 },
        Deallocate { vfd: i32 },
        Duplicate { old_vfd: i32 },
        DuplicateAt { old_vfd: i32, new_vfd: i32 },
    }

    fn lowest_available(allocated: &BTreeMap<i32, i32>) -> i32 {
        let mut fd = FIRST_USER_FD;
        loop {
            if !allocated.contains_key(&fd) {
                return fd;
            }
            fd = fd.checked_add(1).expect("fd overflow in repro model");
        }
    }

    fn lowest_available_min(allocated: &BTreeMap<i32, i32>, min_fd: i32) -> i32 {
        let mut fd = std::cmp::max(FIRST_USER_FD, min_fd);
        loop {
            if !allocated.contains_key(&fd) {
                return fd;
            }
            fd = fd.checked_add(1).expect("fd overflow in repro model");
        }
    }

    fn choose_existing(model: &BTreeMap<i32, i32>, seed: i32, fallback: i32) -> i32 {
        if model.is_empty() {
            return fallback;
        }
        // Sometimes intentionally *don't* pick an existing fd, so we also exercise
        // error paths (e.g., dup/dealloc on non-existent fd).
        if (seed.unsigned_abs() % 3) != 0 {
            return fallback;
        }
        let idx = (seed.unsigned_abs() as usize) % model.len();
        *model.keys().nth(idx).unwrap()
    }

    fn assert_model_matches(table: &FdTable, model: &BTreeMap<i32, i32>) -> Result<(), String> {
        for (&vfd, &kfd) in model {
            let got = table.translate(vfd);
            if got != Some(kfd) {
                return Err(format!(
                    "translate({}) mismatch: expected Some({}), got {:?}",
                    vfd, kfd, got
                ));
            }
        }
        for vfd in FIRST_USER_FD..120 {
            let expected = model.get(&vfd).copied();
            let got = table.translate(vfd);
            if got != expected {
                return Err(format!(
                    "translate({}) mismatch: expected {:?}, got {:?}",
                    vfd, expected, got
                ));
            }
        }
        Ok(())
    }

    pub fn run_case(case: &ReproCase) -> Result<(), String> {
        let mut tables: Vec<FdTable> = vec![FdTable::new()];
        let mut models: Vec<BTreeMap<i32, i32>> = vec![BTreeMap::new()];

        for (step_idx, op) in case.ops.iter().enumerate() {
            let table_idx = match op {
                ReproOp::DeepClone => 0usize,
                ReproOp::Allocate { kernel_fd } => {
                    (kernel_fd.unsigned_abs() as usize) % tables.len()
                }
                ReproOp::AllocateMin { min_fd, .. } => {
                    (min_fd.unsigned_abs() as usize) % tables.len()
                }
                ReproOp::AllocateAt { vfd, .. } => (vfd.unsigned_abs() as usize) % tables.len(),
                ReproOp::Deallocate { vfd } => (vfd.unsigned_abs() as usize) % tables.len(),
                ReproOp::Duplicate { old_vfd } => (old_vfd.unsigned_abs() as usize) % tables.len(),
                ReproOp::DuplicateAt { old_vfd, .. } => {
                    (old_vfd.unsigned_abs() as usize) % tables.len()
                }
            };

            match op {
                ReproOp::DeepClone => {
                    if tables.len() < 4 {
                        let t = tables[table_idx].deep_clone();
                        let m = models[table_idx].clone();
                        tables.push(t);
                        models.push(m);
                    }
                }
                ReproOp::Allocate { kernel_fd } => {
                    let table = &tables[table_idx];
                    let model = &mut models[table_idx];
                    let expected_vfd = lowest_available(model);
                    let got = table.allocate(FdEntry::Passthrough {
                        kernel_fd: *kernel_fd,
                        flags: 0,
                        path: None,
                    });
                    if got != expected_vfd {
                        return Err(format!(
                            "step {}: allocate mismatch: expected {}, got {}",
                            step_idx, expected_vfd, got
                        ));
                    }
                    model.insert(got, *kernel_fd);
                }
                ReproOp::AllocateMin { min_fd, kernel_fd } => {
                    let table = &tables[table_idx];
                    let model = &mut models[table_idx];
                    let derived_min = if !model.is_empty() && (min_fd % 2 == 0) {
                        let base = choose_existing(model, *min_fd, FIRST_USER_FD);
                        std::cmp::max(FIRST_USER_FD, base.saturating_add((min_fd % 11).abs()))
                    } else {
                        *min_fd
                    };

                    let expected_vfd = lowest_available_min(model, derived_min);
                    let got = table.allocate_min(
                        derived_min,
                        FdEntry::Passthrough {
                            kernel_fd: *kernel_fd,
                            flags: 0,
                            path: None,
                        },
                    );
                    if got != expected_vfd {
                        return Err(format!(
                            "step {}: allocate_min(min={}) mismatch: expected {}, got {}",
                            step_idx, derived_min, expected_vfd, got
                        ));
                    }
                    model.insert(got, *kernel_fd);
                }
                ReproOp::AllocateAt { vfd, kernel_fd } => {
                    let table = &tables[table_idx];
                    let model = &mut models[table_idx];
                    let target = choose_existing(model, *vfd, *vfd);
                    let expected_old = model.get(&target).copied();
                    let old = table.allocate_at(
                        target,
                        FdEntry::Passthrough {
                            kernel_fd: *kernel_fd,
                            flags: 0,
                            path: None,
                        },
                    );
                    let got_old = old.as_ref().and_then(|e| e.kernel_fd());
                    if got_old != expected_old {
                        return Err(format!(
                            "step {}: allocate_at({}) old mismatch: expected {:?}, got {:?}",
                            step_idx, target, expected_old, got_old
                        ));
                    }
                    model.insert(target, *kernel_fd);
                }
                ReproOp::Deallocate { vfd } => {
                    let table = &tables[table_idx];
                    let model = &mut models[table_idx];
                    let target = choose_existing(model, *vfd, *vfd);
                    let expected = model.remove(&target);
                    let got = table.deallocate(target).and_then(|e| e.kernel_fd());
                    if got != expected {
                        return Err(format!(
                            "step {}: deallocate({}) mismatch: expected {:?}, got {:?}",
                            step_idx, target, expected, got
                        ));
                    }
                }
                ReproOp::Duplicate { old_vfd } => {
                    let table = &tables[table_idx];
                    let model = &mut models[table_idx];
                    let src = choose_existing(model, *old_vfd, *old_vfd);
                    let expected_kernel = model.get(&src).copied();
                    let got = table.duplicate(src);
                    match (expected_kernel, got) {
                        (None, None) => {}
                        (Some(kfd), Some(new_vfd)) => {
                            let expected_vfd = lowest_available(model);
                            if new_vfd != expected_vfd {
                                return Err(format!(
                                    "step {}: duplicate({}) mismatch: expected new_vfd {}, got {}",
                                    step_idx, src, expected_vfd, new_vfd
                                ));
                            }
                            model.insert(new_vfd, kfd);
                        }
                        (a, b) => {
                            return Err(format!(
                                "step {}: duplicate({}) mismatch: expected {:?}, got {:?}",
                                step_idx, src, a, b
                            ));
                        }
                    }
                }
                ReproOp::DuplicateAt { old_vfd, new_vfd } => {
                    let table = &tables[table_idx];
                    let model = &mut models[table_idx];
                    let src = choose_existing(model, *old_vfd, *old_vfd);
                    let dst = if !model.is_empty() && (new_vfd % 2 == 0) {
                        choose_existing(model, *new_vfd, *new_vfd)
                    } else {
                        *new_vfd
                    };

                    let expected_kernel = model.get(&src).copied();
                    let expected_old_at_dest = model.get(&dst).copied();
                    let old = table.duplicate_at(src, dst);
                    match expected_kernel {
                        None => {
                            if old.is_some() {
                                return Err(format!(
                                    "step {}: duplicate_at({}, {}) should have failed (src missing)",
                                    step_idx, src, dst
                                ));
                            }
                        }
                        Some(kfd) => {
                            let got_old = old.as_ref().and_then(|e| e.kernel_fd());
                            if got_old != expected_old_at_dest {
                                return Err(format!(
                                    "step {}: duplicate_at({}, {}) old mismatch: expected {:?}, got {:?}",
                                    step_idx, src, dst, expected_old_at_dest, got_old
                                ));
                            }
                            model.insert(dst, kfd);
                        }
                    }
                }
            }

            for (t, m) in tables.iter().zip(models.iter()) {
                assert_model_matches(t, m).map_err(|e| format!("step {}: {}", step_idx, e))?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_standard_fds() {
        let table = FdTable::new();

        assert_eq!(table.translate(0), Some(0)); // stdin
        assert_eq!(table.translate(1), Some(1)); // stdout
        assert_eq!(table.translate(2), Some(2)); // stderr
    }

    #[test]
    fn test_allocate() {
        let table = FdTable::new();

        let entry1 = FdEntry::Passthrough {
            kernel_fd: 100,
            flags: 0,
            path: None,
        };
        let vfd1 = table.allocate(entry1);
        assert_eq!(vfd1, 3); // First non-standard FD
        assert_eq!(table.translate(3), Some(100));

        let entry2 = FdEntry::Passthrough {
            kernel_fd: 101,
            flags: 0,
            path: None,
        };
        let vfd2 = table.allocate(entry2);
        assert_eq!(vfd2, 4);
        assert_eq!(table.translate(4), Some(101));
    }

    #[test]
    fn test_deallocate() {
        let table = FdTable::new();

        let entry = FdEntry::Passthrough {
            kernel_fd: 100,
            flags: 0,
            path: None,
        };
        let vfd = table.allocate(entry);
        assert_eq!(table.translate(vfd), Some(100));

        let entry = table.deallocate(vfd);
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().kernel_fd(), Some(100));

        assert_eq!(table.translate(vfd), None);
    }

    #[test]
    fn test_duplicate() {
        let table = FdTable::new();

        let entry = FdEntry::Passthrough {
            kernel_fd: 100,
            flags: 0,
            path: None,
        };
        let vfd1 = table.allocate(entry);
        let vfd2 = table.duplicate(vfd1).unwrap();

        assert_ne!(vfd1, vfd2);
        assert_eq!(table.translate(vfd1), Some(100));
        assert_eq!(table.translate(vfd2), Some(100));
    }

    #[test]
    fn test_duplicate_at() {
        let table = FdTable::new();

        let entry = FdEntry::Passthrough {
            kernel_fd: 100,
            flags: 0,
            path: None,
        };
        let vfd1 = table.allocate(entry);
        let result = table.duplicate_at(vfd1, 10);

        // duplicate_at returns the old FdEntry that was at new_vfd (if any)
        // In this case, there was no previous entry at fd 10, so it returns None
        assert!(result.is_none());
        assert_eq!(table.translate(10), Some(100));
    }
}

/// Property tests for `FdTable` correctness.
///
/// These focus on POSIX-like allocation semantics:
/// - `allocate()` must return the lowest available FD (>= 3)
/// - `allocate_min()` must return the lowest available FD >= `min`
/// - `allocate_at()` overwrites the target FD and returns the previous entry (if any)
#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn mk_entry(kernel_fd: i32) -> FdEntry {
        FdEntry::Passthrough {
            kernel_fd,
            flags: 0,
            path: None,
        }
    }

    fn lowest_available(allocated: &BTreeMap<i32, i32>) -> i32 {
        let mut fd = FIRST_USER_FD;
        loop {
            if !allocated.contains_key(&fd) {
                return fd;
            }
            fd = fd.checked_add(1).expect("fd overflow in test model");
        }
    }

    fn lowest_available_min(allocated: &BTreeMap<i32, i32>, min_fd: i32) -> i32 {
        let mut fd = std::cmp::max(FIRST_USER_FD, min_fd);
        loop {
            if !allocated.contains_key(&fd) {
                return fd;
            }
            fd = fd.checked_add(1).expect("fd overflow in test model");
        }
    }

    #[derive(Debug, Clone)]
    enum Op {
        /// Fork-like event: create an independent copy via `deep_clone()`.
        DeepClone,
        Allocate {
            kernel_fd: i32,
        },
        AllocateMin {
            min_fd: i32,
            kernel_fd: i32,
        },
        AllocateAt {
            vfd: i32,
            kernel_fd: i32,
        },
        Deallocate {
            vfd: i32,
        },
        Duplicate {
            old_vfd: i32,
        },
        DuplicateAt {
            old_vfd: i32,
            new_vfd: i32,
        },
    }

    fn ops_strategy() -> impl Strategy<Value = Vec<Op>> {
        // Keep FD numbers small so we can cheaply assert table/model equivalence
        // over a bounded range, while still allowing "gaps" and overwrites.
        let vfd = FIRST_USER_FD..80i32;
        let min_fd = FIRST_USER_FD..80i32;
        let kernel_fd = 3i32..10_000i32;

        // Randomize the skew (weight profile) per generated test case so we cover
        // multiple "workloads" instead of baking in one distribution.
        //
        // We keep weights bounded so generation is efficient and reproducible.
        // (proptest seeds determine the weights deterministically per case)
        // Note: We avoid 0 weights so we can always build a valid `prop_oneof!`.
        (
            1u32..=10,  // deep_clone
            10u32..=70, // allocate
            10u32..=70, // deallocate
            5u32..=50,  // duplicate
            1u32..=30,  // allocate_min
            1u32..=20,  // allocate_at
            1u32..=20,  // duplicate_at
        )
            .prop_flat_map(move |(w_deep_clone, w_alloc, w_dealloc, w_dup, w_alloc_min, w_alloc_at, w_dup_at)| {
                // Build Op strategy directly (better shrinking than pick-and-map).
                let op = prop_oneof![
                    w_deep_clone => Just(Op::DeepClone),
                    w_alloc => kernel_fd.clone().prop_map(|k| Op::Allocate { kernel_fd: k }),
                    w_dealloc => vfd.clone().prop_map(|fd| Op::Deallocate { vfd: fd }),
                    w_dup => vfd.clone().prop_map(|fd| Op::Duplicate { old_vfd: fd }),
                    w_alloc_min => (min_fd.clone(), kernel_fd.clone()).prop_map(|(m, k)| Op::AllocateMin { min_fd: m, kernel_fd: k }),
                    w_alloc_at => (vfd.clone(), kernel_fd.clone()).prop_map(|(fd, k)| Op::AllocateAt { vfd: fd, kernel_fd: k }),
                    w_dup_at => (vfd.clone(), vfd.clone()).prop_map(|(old, new)| Op::DuplicateAt { old_vfd: old, new_vfd: new }),
                ];

                prop::collection::vec(op, 0..600)
            })
    }

    fn assert_model_matches(table: &FdTable, model: &BTreeMap<i32, i32>) {
        // Check all model FDs translate to expected kernel FDs.
        for (&vfd, &kfd) in model {
            assert_eq!(
                table.translate(vfd),
                Some(kfd),
                "translate({}) mismatch",
                vfd
            );
        }

        // Spot-check a bounded range to ensure we don't have "ghost" entries.
        for vfd in FIRST_USER_FD..120 {
            if let Some(&kfd) = model.get(&vfd) {
                assert_eq!(table.translate(vfd), Some(kfd));
            } else {
                assert_eq!(table.translate(vfd), None);
            }
        }
    }

    proptest! {
        // Default settings can struggle to shrink long, stateful programs.
        // Bump shrink budget so we more reliably get small repros.
        #![proptest_config(ProptestConfig {
            cases: 256,
            max_shrink_iters: 20_000,
            .. ProptestConfig::default()
        })]

        // This is intentionally ignored by default since it is expensive and is expected
        // to find real bugs in the current FD-table implementation.
        //
        // Run with:
        // - `cargo test -p agentfs-sandbox prop_fdtable_allocation_semantics -- --ignored --nocapture`
        #[test]
        #[ignore]
        fn prop_fdtable_allocation_semantics(ops in ops_strategy()) {
            // Keep multiple independent tables around to model fork/clone behavior.
            // (DeepClone creates a new independent FD table snapshot.)
            let mut tables: Vec<FdTable> = vec![FdTable::new()];
            let mut models: Vec<BTreeMap<i32, i32>> = vec![BTreeMap::new()];

            // Emit a replay artifact whenever we hit a failure.
            //
            // By default we only overwrite `last_failure.json` (so shrinking doesn't spam
            // the filesystem). If you want *all* intermediate failing candidates, set:
            // - `AGENTFS_SAVE_ALL_REPROS=1`
            let artifact_dir: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("proptest-artifacts")
                .join("fdtable");

            let write_artifact = |ops: &[Op], step_idx: usize, message: &str| {
                let _ = fs::create_dir_all(&artifact_dir);

                let case = crate::vfs::fdtable::repro::ReproCase {
                    ops: ops.iter().map(|op| match op {
                        Op::DeepClone => crate::vfs::fdtable::repro::ReproOp::DeepClone,
                        Op::Allocate { kernel_fd } => crate::vfs::fdtable::repro::ReproOp::Allocate { kernel_fd: *kernel_fd },
                        Op::AllocateMin { min_fd, kernel_fd } => crate::vfs::fdtable::repro::ReproOp::AllocateMin { min_fd: *min_fd, kernel_fd: *kernel_fd },
                        Op::AllocateAt { vfd, kernel_fd } => crate::vfs::fdtable::repro::ReproOp::AllocateAt { vfd: *vfd, kernel_fd: *kernel_fd },
                        Op::Deallocate { vfd } => crate::vfs::fdtable::repro::ReproOp::Deallocate { vfd: *vfd },
                        Op::Duplicate { old_vfd } => crate::vfs::fdtable::repro::ReproOp::Duplicate { old_vfd: *old_vfd },
                        Op::DuplicateAt { old_vfd, new_vfd } => crate::vfs::fdtable::repro::ReproOp::DuplicateAt { old_vfd: *old_vfd, new_vfd: *new_vfd },
                    }).collect(),
                };

                let last_path = artifact_dir.join("last_failure.json");
                let info_path = artifact_dir.join("last_failure.txt");

                if let Ok(json) = serde_json::to_string_pretty(&case) {
                    let _ = fs::write(&last_path, &json);

                    if std::env::var_os("AGENTFS_SAVE_ALL_REPROS").is_some() {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default();
                        let unique =
                            format!("case-{}-{}.json", now.as_secs(), now.subsec_nanos());
                        let unique_path = artifact_dir.join(&unique);
                        let _ = fs::write(&unique_path, &json);
                    }
                }

                let info = format!(
                    "fdtable proptest failure\n\
                     step: {}\n\
                     message: {}\n\
                     artifact: {}\n\
                     replay: cargo run -p agentfs-sandbox --bin fdtable_replay -- {}\n",
                    step_idx,
                    message,
                    last_path.display(),
                    last_path.display(),
                );
                let _ = fs::write(&info_path, info);
            };

            for (step_idx, op) in ops.iter().enumerate() {
                // Pick an active table index in a stable (seed-driven) way.
                // We re-use the tagged integers produced by the strategy to spread ops.
                let table_idx = match op {
                    Op::DeepClone => 0usize,
                    Op::Allocate { kernel_fd } => (kernel_fd.unsigned_abs() as usize) % tables.len(),
                    Op::AllocateMin { min_fd, .. } => (min_fd.unsigned_abs() as usize) % tables.len(),
                    Op::AllocateAt { vfd, .. } => (vfd.unsigned_abs() as usize) % tables.len(),
                    Op::Deallocate { vfd } => (vfd.unsigned_abs() as usize) % tables.len(),
                    Op::Duplicate { old_vfd } => (old_vfd.unsigned_abs() as usize) % tables.len(),
                    Op::DuplicateAt { old_vfd, .. } => (old_vfd.unsigned_abs() as usize) % tables.len(),
                };

                // Helper to bias selection toward existing FDs (more realistic).
                // Uses an input "seed" (an int embedded in the op) to pick a stable key.
                let choose_existing = |model: &BTreeMap<i32, i32>, seed: i32, fallback: i32| -> i32 {
                    if model.is_empty() {
                        return fallback;
                    }
                    // Sometimes intentionally *don't* pick an existing fd, so we also exercise
                    // error paths (e.g., dup/dealloc on non-existent fd).
                    if (seed.unsigned_abs() % 3) != 0 {
                        return fallback;
                    }
                    let idx = (seed.unsigned_abs() as usize) % model.len();
                    *model.keys().nth(idx).unwrap()
                };

                match op.clone() {
                    Op::DeepClone => {
                        // Fork-like: clone the currently selected table/model.
                        // Keep fan-out bounded so tests don't get too slow.
                        if tables.len() < 4 {
                            // Clone from the selected table index.
                            let t = tables[table_idx].deep_clone();
                            let m = models[table_idx].clone();
                            tables.push(t);
                            models.push(m);
                        }
                    }
                    Op::Allocate { kernel_fd } => {
                        let table = &tables[table_idx];
                        let model = &mut models[table_idx];
                        let expected_vfd = lowest_available(model);
                        let got = table.allocate(mk_entry(kernel_fd));
                        if got != expected_vfd {
                            let msg = format!("allocate mismatch: expected {}, got {}", expected_vfd, got);
                            write_artifact(&ops, step_idx, &msg);
                            return Err(TestCaseError::fail(msg));
                        }
                        model.insert(got, kernel_fd);
                    }
                    Op::AllocateMin { min_fd, kernel_fd } => {
                        let table = &tables[table_idx];
                        let model = &mut models[table_idx];
                        // Make allocate_min interact with existing state by sometimes using
                        // min_fd relative to an existing FD (classic dupfd patterns).
                        let derived_min = if !model.is_empty() && (min_fd % 2 == 0) {
                            let base = choose_existing(model, min_fd, FIRST_USER_FD);
                            std::cmp::max(FIRST_USER_FD, base.saturating_add((min_fd % 11).abs()))
                        } else {
                            min_fd
                        };

                        let expected_vfd = lowest_available_min(model, derived_min);
                        let got = table.allocate_min(derived_min, mk_entry(kernel_fd));
                        if got != expected_vfd {
                            let msg = format!(
                                "allocate_min(min={}) mismatch: expected {}, got {}",
                                derived_min, expected_vfd, got
                            );
                            write_artifact(&ops, step_idx, &msg);
                            return Err(TestCaseError::fail(msg));
                        }
                        model.insert(got, kernel_fd);
                    }
                    Op::AllocateAt { vfd, kernel_fd } => {
                        let table = &tables[table_idx];
                        let model = &mut models[table_idx];
                        let target = choose_existing(model, vfd, vfd);
                        let expected_old = model.get(&target).copied();
                        let old = table.allocate_at(target, mk_entry(kernel_fd));
                        match (expected_old, old) {
                            (None, None) => {}
                            (Some(k), Some(e)) => {
                                let got_old = e.kernel_fd();
                                if got_old != Some(k) {
                                    let msg = format!(
                                        "allocate_at({}) old mismatch: expected {:?}, got {:?}",
                                        target,
                                        Some(k),
                                        got_old
                                    );
                                    write_artifact(&ops, step_idx, &msg);
                                    return Err(TestCaseError::fail(msg));
                                }
                            }
                            (a, b) => {
                                let msg = format!(
                                    "allocate_at({}) old mismatch: expected {:?}, got {:?}",
                                    target,
                                    a,
                                    b.as_ref().and_then(|e| e.kernel_fd())
                                );
                                write_artifact(&ops, step_idx, &msg);
                                return Err(TestCaseError::fail(msg));
                            }
                        }
                        model.insert(target, kernel_fd);
                    }
                    Op::Deallocate { vfd } => {
                        let table = &tables[table_idx];
                        let model = &mut models[table_idx];
                        let target = choose_existing(model, vfd, vfd);
                        let expected = model.remove(&target);
                        let got = table.deallocate(target).and_then(|e| e.kernel_fd());
                        if got != expected {
                            let msg = format!(
                                "deallocate({}) mismatch: expected {:?}, got {:?}",
                                target, expected, got
                            );
                            write_artifact(&ops, step_idx, &msg);
                            return Err(TestCaseError::fail(msg));
                        }
                    }
                    Op::Duplicate { old_vfd } => {
                        let table = &tables[table_idx];
                        let model = &mut models[table_idx];
                        let src = choose_existing(model, old_vfd, old_vfd);
                        let expected_kernel = model.get(&src).copied();
                        let got = table.duplicate(src);

                        match (expected_kernel, got) {
                            (None, None) => {}
                            (Some(kfd), Some(new_vfd)) => {
                                // dup allocates a new FD at the lowest available number.
                                let expected_vfd = lowest_available(model);
                                if new_vfd != expected_vfd {
                                    let msg = format!(
                                        "duplicate({}) mismatch: expected new_vfd {}, got {}",
                                        src, expected_vfd, new_vfd
                                    );
                                    write_artifact(&ops, step_idx, &msg);
                                    return Err(TestCaseError::fail(msg));
                                }
                                model.insert(new_vfd, kfd);
                            }
                            (a, b) => {
                                let msg = format!(
                                    "duplicate({}) mismatch: expected {:?}, got {:?}",
                                    src,
                                    a.map(|_| "Some"),
                                    b.map(|_| "Some")
                                );
                                write_artifact(&ops, step_idx, &msg);
                                return Err(TestCaseError::fail(msg));
                            }
                        }
                    }
                    Op::DuplicateAt { old_vfd, new_vfd } => {
                        let table = &tables[table_idx];
                        let model = &mut models[table_idx];
                        let src = choose_existing(model, old_vfd, old_vfd);
                        // For the destination, also bias toward existing FDs sometimes
                        // to force overwrite paths, but allow new FDs as well.
                        let dst = if !model.is_empty() && (new_vfd % 2 == 0) {
                            choose_existing(model, new_vfd, new_vfd)
                        } else {
                            new_vfd
                        };

                        let expected_kernel = model.get(&src).copied();
                        let expected_old_at_dest = model.get(&dst).copied();

                        let old = table.duplicate_at(src, dst);

                        match expected_kernel {
                            None => {
                                if old.is_some() {
                                    let msg = format!(
                                        "duplicate_at({}, {}) should have failed (src missing)",
                                        src, dst
                                    );
                                    write_artifact(&ops, step_idx, &msg);
                                    return Err(TestCaseError::fail(msg));
                                }
                                // Model unchanged
                            }
                            Some(kfd) => {
                                // If dest had something, duplicate_at returns it.
                                match (expected_old_at_dest, old) {
                                    (None, None) => {}
                                    (Some(k), Some(e)) => {
                                        let got_old = e.kernel_fd();
                                        if got_old != Some(k) {
                                            let msg = format!(
                                                "duplicate_at({}, {}) old mismatch: expected {:?}, got {:?}",
                                                src, dst, Some(k), got_old
                                            );
                                            write_artifact(&ops, step_idx, &msg);
                                            return Err(TestCaseError::fail(msg));
                                        }
                                    }
                                    (a, b) => {
                                        let msg = format!(
                                            "duplicate_at({}, {}) old mismatch: expected {:?}, got {:?}",
                                            src,
                                            dst,
                                            a,
                                            b.as_ref().and_then(|e| e.kernel_fd())
                                        );
                                        write_artifact(&ops, step_idx, &msg);
                                        return Err(TestCaseError::fail(msg));
                                    }
                                }
                                model.insert(dst, kfd);
                            }
                        }
                    }
                }

                // After each step, the observable translation must match the model for ALL tables.
                for (t, m) in tables.iter().zip(models.iter()) {
                    // On model mismatch, emit a repro artifact and fail.
                    // Note: `assert_model_matches` panics; we convert to a proptest failure
                    // so shrinking can proceed.
                    let result = std::panic::catch_unwind(|| assert_model_matches(t, m));
                    if result.is_err() {
                        let msg = "model mismatch (see panic output / replay artifact)".to_string();
                        write_artifact(&ops, step_idx, &msg);
                        return Err(TestCaseError::fail(msg));
                    }
                }
            }
        }
    }
}
