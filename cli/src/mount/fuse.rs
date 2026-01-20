//! FUSE backend implementation for the mount infrastructure.

use anyhow::Result;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::{wait_for_mount, MountBackend, MountHandle, MountHandleInner, MountOpts};

/// FUSE unmount implementation using fusermount.
#[cfg(target_os = "linux")]
pub(super) fn unmount_fuse(mountpoint: &Path, lazy: bool) -> Result<()> {
    const FUSERMOUNT_COMMANDS: &[&str] = &["fusermount3", "fusermount"];
    let args: &[&str] = if lazy { &["-uz"] } else { &["-u"] };

    for cmd in FUSERMOUNT_COMMANDS {
        let result = Command::new(cmd)
            .args(args)
            .arg(mountpoint.as_os_str())
            .status();

        match result {
            Ok(status) if status.success() => return Ok(()),
            Ok(_) => continue,
            Err(_) => continue,
        }
    }

    anyhow::bail!(
        "Failed to unmount {}. You may need to unmount manually with: fusermount -u {}",
        mountpoint.display(),
        mountpoint.display()
    )
}

/// FUSE unmount is not available on macOS.
#[cfg(target_os = "macos")]
pub(super) fn unmount_fuse(_mountpoint: &Path, _lazy: bool) -> Result<()> {
    anyhow::bail!("FUSE unmount is not supported on macOS")
}

/// Internal FUSE mount implementation.
#[cfg(target_os = "linux")]
pub(super) fn mount_fuse(
    fs: Arc<Mutex<dyn agentfs_sdk::FileSystem + Send>>,
    opts: MountOpts,
) -> Result<MountHandle> {
    use crate::fuse::FuseMountOptions;

    let fuse_opts = FuseMountOptions {
        mountpoint: opts.mountpoint.clone(),
        auto_unmount: opts.auto_unmount,
        allow_root: opts.allow_root,
        allow_other: opts.allow_other,
        fsname: opts.fsname.clone(),
        uid: opts.uid,
        gid: opts.gid,
    };

    let mountpoint = opts.mountpoint.clone();
    let timeout = opts.timeout;
    let lazy_unmount = opts.lazy_unmount;

    let fs_adapter = MutexFsAdapter { inner: fs };
    let fs_arc: Arc<dyn agentfs_sdk::FileSystem> = Arc::new(fs_adapter);

    let fuse_handle = std::thread::spawn(move || {
        let rt = crate::get_runtime();
        crate::fuse::mount(fs_arc, fuse_opts, rt)
    });

    if !wait_for_mount(&mountpoint, timeout) {
        anyhow::bail!("FUSE mount did not become ready within {:?}", timeout);
    }

    Ok(MountHandle {
        mountpoint,
        backend: MountBackend::Fuse,
        lazy_unmount,
        inner: MountHandleInner::Fuse {
            _thread: fuse_handle,
        },
    })
}

/// Adapter to use `Arc<Mutex<dyn FileSystem>>` as `Arc<dyn FileSystem>`.
struct MutexFsAdapter {
    inner: Arc<Mutex<dyn agentfs_sdk::FileSystem + Send>>,
}

#[async_trait::async_trait]
impl agentfs_sdk::FileSystem for MutexFsAdapter {
    async fn stat(
        &self,
        path: &str,
    ) -> std::result::Result<Option<agentfs_sdk::Stats>, agentfs_sdk::error::Error> {
        self.inner.lock().await.stat(path).await
    }

    async fn lstat(
        &self,
        path: &str,
    ) -> std::result::Result<Option<agentfs_sdk::Stats>, agentfs_sdk::error::Error> {
        self.inner.lock().await.lstat(path).await
    }

    async fn read_file(
        &self,
        path: &str,
    ) -> std::result::Result<Option<Vec<u8>>, agentfs_sdk::error::Error> {
        self.inner.lock().await.read_file(path).await
    }

    async fn readdir(
        &self,
        path: &str,
    ) -> std::result::Result<Option<Vec<String>>, agentfs_sdk::error::Error> {
        self.inner.lock().await.readdir(path).await
    }

    async fn readdir_plus(
        &self,
        path: &str,
    ) -> std::result::Result<Option<Vec<agentfs_sdk::DirEntry>>, agentfs_sdk::error::Error> {
        self.inner.lock().await.readdir_plus(path).await
    }

    async fn readlink(
        &self,
        path: &str,
    ) -> std::result::Result<Option<String>, agentfs_sdk::error::Error> {
        self.inner.lock().await.readlink(path).await
    }

    async fn open(
        &self,
        path: &str,
    ) -> std::result::Result<agentfs_sdk::BoxedFile, agentfs_sdk::error::Error> {
        self.inner.lock().await.open(path).await
    }

    async fn create_file(
        &self,
        path: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<(agentfs_sdk::Stats, agentfs_sdk::BoxedFile), agentfs_sdk::error::Error>
    {
        self.inner
            .lock()
            .await
            .create_file(path, mode, uid, gid)
            .await
    }

    async fn mkdir(
        &self,
        path: &str,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.mkdir(path, uid, gid).await
    }

    async fn mknod(
        &self,
        path: &str,
        mode: u32,
        rdev: u64,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner
            .lock()
            .await
            .mknod(path, mode, rdev, uid, gid)
            .await
    }

    async fn remove(&self, path: &str) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.remove(path).await
    }

    async fn rename(
        &self,
        from: &str,
        to: &str,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.rename(from, to).await
    }

    async fn symlink(
        &self,
        target: &str,
        link_path: &str,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner
            .lock()
            .await
            .symlink(target, link_path, uid, gid)
            .await
    }

    async fn link(
        &self,
        old_path: &str,
        new_path: &str,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.link(old_path, new_path).await
    }

    async fn chmod(
        &self,
        path: &str,
        mode: u32,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.chmod(path, mode).await
    }

    async fn chown(
        &self,
        path: &str,
        uid: Option<u32>,
        gid: Option<u32>,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.chown(path, uid, gid).await
    }

    async fn statfs(
        &self,
    ) -> std::result::Result<agentfs_sdk::FilesystemStats, agentfs_sdk::error::Error> {
        self.inner.lock().await.statfs().await
    }
}
