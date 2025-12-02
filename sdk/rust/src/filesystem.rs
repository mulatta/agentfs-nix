use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use turso::{Builder, Connection, Value};

// File types for mode field
const S_IFMT: u32 = 0o170000; // File type mask
const S_IFREG: u32 = 0o100000; // Regular file
const S_IFDIR: u32 = 0o040000; // Directory
const S_IFLNK: u32 = 0o120000; // Symbolic link

// Default permissions
const DEFAULT_FILE_MODE: u32 = S_IFREG | 0o644; // Regular file, rw-r--r--
const DEFAULT_DIR_MODE: u32 = S_IFDIR | 0o755; // Directory, rwxr-xr-x

const ROOT_INO: i64 = 1;
const DEFAULT_CHUNK_SIZE: usize = 4096;

/// File statistics
#[derive(Debug, Clone)]
pub struct Stats {
    pub ino: i64,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: i64,
    pub atime: i64,
    pub mtime: i64,
    pub ctime: i64,
}

impl Stats {
    pub fn is_file(&self) -> bool {
        (self.mode & S_IFMT) == S_IFREG
    }

    pub fn is_directory(&self) -> bool {
        (self.mode & S_IFMT) == S_IFDIR
    }

    pub fn is_symlink(&self) -> bool {
        (self.mode & S_IFMT) == S_IFLNK
    }
}

/// A filesystem backed by SQLite
#[derive(Clone)]
pub struct Filesystem {
    conn: Arc<Connection>,
    chunk_size: usize,
}

impl Filesystem {
    /// Create a new filesystem
    pub async fn new(db_path: &str) -> Result<Self> {
        let db = Builder::new_local(db_path).build().await?;
        let conn = Arc::new(db.connect()?);
        Self::from_connection(conn).await
    }

    /// Create a filesystem from an existing connection
    pub async fn from_connection(conn: Arc<Connection>) -> Result<Self> {
        // Initialize schema first
        Self::initialize_schema(&conn).await?;

        // Get chunk_size from config (or use default)
        let chunk_size = Self::read_chunk_size(&conn).await?;

        let fs = Self { conn, chunk_size };
        Ok(fs)
    }

    /// Get the configured chunk size
    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    /// Initialize the database schema
    async fn initialize_schema(conn: &Connection) -> Result<()> {
        // Create config table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS fs_config (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
            (),
        )
        .await?;

        // Create inode table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS fs_inode (
                ino INTEGER PRIMARY KEY AUTOINCREMENT,
                mode INTEGER NOT NULL,
                uid INTEGER NOT NULL DEFAULT 0,
                gid INTEGER NOT NULL DEFAULT 0,
                size INTEGER NOT NULL DEFAULT 0,
                atime INTEGER NOT NULL,
                mtime INTEGER NOT NULL,
                ctime INTEGER NOT NULL
            )",
            (),
        )
        .await?;

        // Create directory entry table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS fs_dentry (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                parent_ino INTEGER NOT NULL,
                ino INTEGER NOT NULL,
                UNIQUE(parent_ino, name)
            )",
            (),
        )
        .await?;

        // Create index for efficient path lookups
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_fs_dentry_parent
            ON fs_dentry(parent_ino, name)",
            (),
        )
        .await?;

        // Create data chunks table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS fs_data (
                ino INTEGER NOT NULL,
                chunk_index INTEGER NOT NULL,
                data BLOB NOT NULL,
                PRIMARY KEY (ino, chunk_index)
            )",
            (),
        )
        .await?;

        // Create symlink table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS fs_symlink (
                ino INTEGER PRIMARY KEY,
                target TEXT NOT NULL
            )",
            (),
        )
        .await?;

        // Ensure chunk_size config exists
        let mut rows = conn
            .query("SELECT value FROM fs_config WHERE key = 'chunk_size'", ())
            .await?;

        if rows.next().await?.is_none() {
            conn.execute(
                "INSERT INTO fs_config (key, value) VALUES ('chunk_size', ?)",
                (DEFAULT_CHUNK_SIZE.to_string(),),
            )
            .await?;
        }

        // Ensure root directory exists
        let mut rows = conn
            .query("SELECT ino FROM fs_inode WHERE ino = ?", (ROOT_INO,))
            .await?;

        if rows.next().await?.is_none() {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
            conn.execute(
                "INSERT INTO fs_inode (ino, mode, uid, gid, size, atime, mtime, ctime)
                VALUES (?, ?, 0, 0, 0, ?, ?, ?)",
                (ROOT_INO, DEFAULT_DIR_MODE as i64, now, now, now),
            )
            .await?;
        }

        Ok(())
    }

    /// Read chunk size from config
    async fn read_chunk_size(conn: &Connection) -> Result<usize> {
        let mut rows = conn
            .query("SELECT value FROM fs_config WHERE key = 'chunk_size'", ())
            .await?;

        if let Some(row) = rows.next().await? {
            let value = row
                .get_value(0)
                .ok()
                .and_then(|v| match v {
                    Value::Text(s) => s.parse::<usize>().ok(),
                    Value::Integer(i) => Some(i as usize),
                    _ => None,
                })
                .unwrap_or(DEFAULT_CHUNK_SIZE);
            Ok(value)
        } else {
            Ok(DEFAULT_CHUNK_SIZE)
        }
    }

    /// Normalize a path
    fn normalize_path(&self, path: &str) -> String {
        let normalized = path.trim_end_matches('/');
        let normalized = if normalized.is_empty() {
            "/"
        } else if normalized.starts_with('/') {
            normalized
        } else {
            return format!("/{}", normalized);
        };

        // Handle . and .. components
        let components: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
        let mut result = Vec::new();

        for component in components {
            match component {
                "." => {
                    // Current directory - skip it
                    continue;
                }
                ".." => {
                    // Parent directory - only pop if there is a component to pop (don't traverse above root)
                    if !result.is_empty() {
                        result.pop();
                    }
                }
                _ => {
                    result.push(component);
                }
            }
        }

        if result.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", result.join("/"))
        }
    }

    /// Split path into components
    fn split_path(&self, path: &str) -> Vec<String> {
        let normalized = self.normalize_path(path);
        if normalized == "/" {
            return vec![];
        }
        normalized
            .split('/')
            .filter(|p| !p.is_empty())
            .map(|s| s.to_string())
            .collect()
    }

    /// Get link count for an inode
    async fn get_link_count(&self, ino: i64) -> Result<u32> {
        let mut rows = self
            .conn
            .query(
                "SELECT COUNT(*) as count FROM fs_dentry WHERE ino = ?",
                (ino,),
            )
            .await?;

        if let Some(row) = rows.next().await? {
            let count = row
                .get_value(0)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0);
            Ok(count as u32)
        } else {
            Ok(0)
        }
    }

    /// Build a Stats object from a database row
    ///
    /// The row should contain columns in this order:
    /// ino, mode, uid, gid, size, atime, mtime, ctime
    async fn build_stats_from_row(&self, row: &turso::Row, ino: i64) -> Result<Stats> {
        let nlink = self.get_link_count(ino).await?;
        Ok(Stats {
            ino,
            mode: row
                .get_value(1)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0) as u32,
            nlink,
            uid: row
                .get_value(2)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0) as u32,
            gid: row
                .get_value(3)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0) as u32,
            size: row
                .get_value(4)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0),
            atime: row
                .get_value(5)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0),
            mtime: row
                .get_value(6)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0),
            ctime: row
                .get_value(7)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0),
        })
    }

    /// Resolve a path to an inode number
    async fn resolve_path(&self, path: &str) -> Result<Option<i64>> {
        let components = self.split_path(path);
        if components.is_empty() {
            return Ok(Some(ROOT_INO));
        }

        let mut current_ino = ROOT_INO;
        for component in components {
            let mut rows = self
                .conn
                .query(
                    "SELECT ino FROM fs_dentry WHERE parent_ino = ? AND name = ?",
                    (current_ino, component.as_str()),
                )
                .await?;

            if let Some(row) = rows.next().await? {
                current_ino = row
                    .get_value(0)
                    .ok()
                    .and_then(|v| v.as_integer().copied())
                    .unwrap_or(0);
            } else {
                return Ok(None);
            }
        }

        Ok(Some(current_ino))
    }

    /// Get file statistics without following symlinks
    pub async fn lstat(&self, path: &str) -> Result<Option<Stats>> {
        let path = self.normalize_path(path);
        let ino = match self.resolve_path(&path).await? {
            Some(ino) => ino,
            None => return Ok(None),
        };

        let mut rows = self
            .conn
            .query(
                "SELECT ino, mode, uid, gid, size, atime, mtime, ctime FROM fs_inode WHERE ino = ?",
                (ino,),
            )
            .await?;

        if let Some(row) = rows.next().await? {
            let ino_val = row
                .get_value(0)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0);

            let stats = self.build_stats_from_row(&row, ino_val).await?;
            Ok(Some(stats))
        } else {
            Ok(None)
        }
    }

    /// Get file statistics, following symlinks
    pub async fn stat(&self, path: &str) -> Result<Option<Stats>> {
        let path = self.normalize_path(path);

        // Follow symlinks with a maximum depth to prevent infinite loops
        let mut current_path = path;
        let max_symlink_depth = 40; // Standard limit for symlink following

        for _ in 0..max_symlink_depth {
            let ino = match self.resolve_path(&current_path).await? {
                Some(ino) => ino,
                None => return Ok(None),
            };

            let mut rows = self
                .conn
                .query(
                    "SELECT ino, mode, uid, gid, size, atime, mtime, ctime FROM fs_inode WHERE ino = ?",
                    (ino,),
                )
                .await?;

            if let Some(row) = rows.next().await? {
                let ino_val = row
                    .get_value(0)
                    .ok()
                    .and_then(|v| v.as_integer().copied())
                    .unwrap_or(0);

                let mode = row
                    .get_value(1)
                    .ok()
                    .and_then(|v| v.as_integer().copied())
                    .unwrap_or(0) as u32;

                // Check if this is a symlink
                if (mode & S_IFMT) == S_IFLNK {
                    // Read the symlink target
                    let target = self
                        .readlink(&current_path)
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("Symlink has no target"))?;

                    // Resolve target path (handle both absolute and relative paths)
                    current_path = if target.starts_with('/') {
                        target
                    } else {
                        // Relative path - resolve relative to the symlink's directory
                        let base_path = Path::new(&current_path);
                        let parent = base_path.parent().unwrap_or(Path::new("/"));
                        let joined = parent.join(&target);
                        joined.to_string_lossy().into_owned()
                    };
                    current_path = self.normalize_path(&current_path);
                    continue; // Follow the symlink
                }

                // Not a symlink, return the stats
                let stats = self.build_stats_from_row(&row, ino_val).await?;
                return Ok(Some(stats));
            } else {
                return Ok(None);
            }
        }

        // Too many symlinks
        anyhow::bail!("Too many levels of symbolic links")
    }

    /// Create a directory
    pub async fn mkdir(&self, path: &str) -> Result<()> {
        let path = self.normalize_path(path);
        let components = self.split_path(&path);

        if components.is_empty() {
            anyhow::bail!("Cannot create root directory");
        }

        let parent_path = if components.len() == 1 {
            "/".to_string()
        } else {
            format!("/{}", components[..components.len() - 1].join("/"))
        };

        let parent_ino = self
            .resolve_path(&parent_path)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Parent directory does not exist"))?;

        let name = components.last().unwrap();

        // Check if already exists
        if (self.resolve_path(&path).await?).is_some() {
            anyhow::bail!("Directory already exists");
        }

        // Create inode
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        self.conn
            .execute(
                "INSERT INTO fs_inode (mode, uid, gid, size, atime, mtime, ctime)
                VALUES (?, 0, 0, 0, ?, ?, ?)",
                (DEFAULT_DIR_MODE as i64, now, now, now),
            )
            .await?;

        let mut rows = self.conn.query("SELECT last_insert_rowid()", ()).await?;
        let ino = if let Some(row) = rows.next().await? {
            row.get_value(0)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .ok_or_else(|| anyhow::anyhow!("Failed to get inode"))?
        } else {
            anyhow::bail!("Failed to get inode");
        };

        // Create directory entry
        self.conn
            .execute(
                "INSERT INTO fs_dentry (name, parent_ino, ino) VALUES (?, ?, ?)",
                (name.as_str(), parent_ino, ino),
            )
            .await?;

        Ok(())
    }

    /// Write data to a file
    pub async fn write_file(&self, path: &str, data: &[u8]) -> Result<()> {
        let path = self.normalize_path(path);
        let components = self.split_path(&path);

        if components.is_empty() {
            anyhow::bail!("Cannot write to root directory");
        }

        let parent_path = if components.len() == 1 {
            "/".to_string()
        } else {
            format!("/{}", components[..components.len() - 1].join("/"))
        };

        let parent_ino = self
            .resolve_path(&parent_path)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Parent directory does not exist"))?;

        let name = components.last().unwrap();

        // Check if file exists
        let ino = if let Some(ino) = self.resolve_path(&path).await? {
            // Delete existing data
            self.conn
                .execute("DELETE FROM fs_data WHERE ino = ?", (ino,))
                .await?;
            ino
        } else {
            // Create new inode
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
            self.conn
                .execute(
                    "INSERT INTO fs_inode (mode, uid, gid, size, atime, mtime, ctime)
                    VALUES (?, 0, 0, ?, ?, ?, ?)",
                    (DEFAULT_FILE_MODE as i64, data.len() as i64, now, now, now),
                )
                .await?;

            let mut rows = self.conn.query("SELECT last_insert_rowid()", ()).await?;
            let ino = if let Some(row) = rows.next().await? {
                row.get_value(0)
                    .ok()
                    .and_then(|v| v.as_integer().copied())
                    .ok_or_else(|| anyhow::anyhow!("Failed to get inode"))?
            } else {
                anyhow::bail!("Failed to get inode");
            };

            // Create directory entry
            self.conn
                .execute(
                    "INSERT INTO fs_dentry (name, parent_ino, ino) VALUES (?, ?, ?)",
                    (name.as_str(), parent_ino, ino),
                )
                .await?;

            ino
        };

        // Write data in chunks
        if !data.is_empty() {
            for (chunk_index, chunk) in data.chunks(self.chunk_size).enumerate() {
                self.conn
                    .execute(
                        "INSERT INTO fs_data (ino, chunk_index, data) VALUES (?, ?, ?)",
                        (ino, chunk_index as i64, chunk),
                    )
                    .await?;
            }
        }

        // Update size and mtime
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        self.conn
            .execute(
                "UPDATE fs_inode SET size = ?, mtime = ? WHERE ino = ?",
                (data.len() as i64, now, ino),
            )
            .await?;

        Ok(())
    }

    /// Read data from a file
    pub async fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let ino = match self.resolve_path(path).await? {
            Some(ino) => ino,
            None => return Ok(None),
        };

        let mut rows = self
            .conn
            .query(
                "SELECT data FROM fs_data WHERE ino = ? ORDER BY chunk_index",
                (ino,),
            )
            .await?;

        let mut data = Vec::new();
        while let Some(row) = rows.next().await? {
            if let Ok(Value::Blob(chunk)) = row.get_value(0) {
                data.extend_from_slice(&chunk);
            }
        }

        Ok(Some(data))
    }

    /// List directory contents
    pub async fn readdir(&self, path: &str) -> Result<Option<Vec<String>>> {
        let ino = match self.resolve_path(path).await? {
            Some(ino) => ino,
            None => return Ok(None),
        };

        let mut rows = self
            .conn
            .query(
                "SELECT name FROM fs_dentry WHERE parent_ino = ? ORDER BY name",
                (ino,),
            )
            .await?;

        let mut entries = Vec::new();
        while let Some(row) = rows.next().await? {
            let name = row
                .get_value(0)
                .ok()
                .and_then(|v| {
                    if let Value::Text(s) = v {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            if !name.is_empty() {
                entries.push(name);
            }
        }

        Ok(Some(entries))
    }

    /// Create a symbolic link
    pub async fn symlink(&self, target: &str, linkpath: &str) -> Result<()> {
        let linkpath = self.normalize_path(linkpath);
        let components = self.split_path(&linkpath);

        if components.is_empty() {
            anyhow::bail!("Cannot create symlink at root");
        }

        // Get parent directory
        let parent_path = if components.len() == 1 {
            "/".to_string()
        } else {
            format!("/{}", components[..components.len() - 1].join("/"))
        };

        let parent_ino = self
            .resolve_path(&parent_path)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Parent directory does not exist"))?;

        let name = components.last().unwrap();

        // Check if entry already exists
        if (self.resolve_path(&linkpath).await?).is_some() {
            anyhow::bail!("Path already exists");
        }

        // Create inode for symlink
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let mode = S_IFLNK | 0o777; // Symlinks typically have 777 permissions
        let size = target.len() as i64;

        self.conn
            .execute(
                "INSERT INTO fs_inode (mode, uid, gid, size, atime, mtime, ctime)
                 VALUES (?, 0, 0, ?, ?, ?, ?)",
                (mode, size, now, now, now),
            )
            .await?;

        // Get the newly created inode
        let mut rows = self.conn.query("SELECT last_insert_rowid()", ()).await?;

        let ino = if let Some(row) = rows.next().await? {
            row.get_value(0)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0)
        } else {
            anyhow::bail!("Failed to get new inode");
        };

        // Store symlink target
        self.conn
            .execute(
                "INSERT INTO fs_symlink (ino, target) VALUES (?, ?)",
                (ino, target),
            )
            .await?;

        // Create directory entry
        self.conn
            .execute(
                "INSERT INTO fs_dentry (name, parent_ino, ino) VALUES (?, ?, ?)",
                (name.as_str(), parent_ino, ino),
            )
            .await?;

        Ok(())
    }

    /// Read the target of a symbolic link
    pub async fn readlink(&self, path: &str) -> Result<Option<String>> {
        let path = self.normalize_path(path);

        let ino = match self.resolve_path(&path).await? {
            Some(ino) => ino,
            None => return Ok(None),
        };

        // Check if it's a symlink by querying the inode
        let mut rows = self
            .conn
            .query("SELECT mode FROM fs_inode WHERE ino = ?", (ino,))
            .await?;

        if let Some(row) = rows.next().await? {
            let mode = row
                .get_value(0)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0) as u32;

            // Check if it's a symlink
            if (mode & S_IFMT) != S_IFLNK {
                anyhow::bail!("Not a symbolic link");
            }
        } else {
            return Ok(None);
        }

        // Read target from fs_symlink table
        let mut rows = self
            .conn
            .query("SELECT target FROM fs_symlink WHERE ino = ?", (ino,))
            .await?;

        if let Some(row) = rows.next().await? {
            let target = row
                .get_value(0)
                .ok()
                .and_then(|v| match v {
                    Value::Text(s) => Some(s.to_string()),
                    _ => None,
                })
                .ok_or_else(|| anyhow::anyhow!("Invalid symlink target"))?;
            Ok(Some(target))
        } else {
            Ok(None)
        }
    }

    /// Remove a file or empty directory
    pub async fn remove(&self, path: &str) -> Result<()> {
        let path = self.normalize_path(path);
        let components = self.split_path(&path);

        if components.is_empty() {
            anyhow::bail!("Cannot remove root directory");
        }

        let ino = self
            .resolve_path(&path)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Path does not exist"))?;

        if ino == ROOT_INO {
            anyhow::bail!("Cannot remove root directory");
        }

        // Check if directory is empty
        let mut rows = self
            .conn
            .query(
                "SELECT COUNT(*) FROM fs_dentry WHERE parent_ino = ?",
                (ino,),
            )
            .await?;

        if let Some(row) = rows.next().await? {
            let count = row
                .get_value(0)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0);
            if count > 0 {
                anyhow::bail!("Directory not empty");
            }
        }

        // Get parent directory and name
        let parent_path = if components.len() == 1 {
            "/".to_string()
        } else {
            format!("/{}", components[..components.len() - 1].join("/"))
        };

        let parent_ino = self
            .resolve_path(&parent_path)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Parent directory does not exist"))?;

        let name = components.last().unwrap();

        // Delete the specific directory entry (not all entries pointing to this inode)
        self.conn
            .execute(
                "DELETE FROM fs_dentry WHERE parent_ino = ? AND name = ?",
                (parent_ino, name.as_str()),
            )
            .await?;

        // Check if this was the last link to the inode
        let link_count = self.get_link_count(ino).await?;
        if link_count == 0 {
            // Manually handle cascading deletes since we don't use foreign keys
            // Delete data blocks
            self.conn
                .execute("DELETE FROM fs_data WHERE ino = ?", (ino,))
                .await?;

            // Delete symlink if exists
            self.conn
                .execute("DELETE FROM fs_symlink WHERE ino = ?", (ino,))
                .await?;

            // Delete inode
            self.conn
                .execute("DELETE FROM fs_inode WHERE ino = ?", (ino,))
                .await?;
        }

        Ok(())
    }

    /// Get the number of chunks for a given inode (for testing)
    #[cfg(test)]
    async fn get_chunk_count(&self, ino: i64) -> Result<i64> {
        let mut rows = self
            .conn
            .query("SELECT COUNT(*) FROM fs_data WHERE ino = ?", (ino,))
            .await?;

        if let Some(row) = rows.next().await? {
            Ok(row
                .get_value(0)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0))
        } else {
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn create_test_fs() -> Result<(Filesystem, tempfile::TempDir)> {
        let dir = tempdir()?;
        let db_path = dir.path().join("test.db");
        let fs = Filesystem::new(db_path.to_str().unwrap()).await?;
        Ok((fs, dir))
    }

    // ==================== Chunk Size Boundary Tests ====================

    #[tokio::test]
    async fn test_file_smaller_than_chunk_size() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        // Write a file smaller than chunk_size (100 bytes)
        let data = vec![0u8; 100];
        fs.write_file("/small.txt", &data).await?;

        // Read it back
        let read_data = fs.read_file("/small.txt").await?.unwrap();
        assert_eq!(read_data.len(), 100);
        assert_eq!(read_data, data);

        // Verify only 1 chunk was created
        let ino = fs.resolve_path("/small.txt").await?.unwrap();
        let chunk_count = fs.get_chunk_count(ino).await?;
        assert_eq!(chunk_count, 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_file_exactly_chunk_size() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        // Write exactly chunk_size bytes
        let chunk_size = fs.chunk_size();
        let data: Vec<u8> = (0..chunk_size).map(|i| (i % 256) as u8).collect();
        fs.write_file("/exact.txt", &data).await?;

        // Read it back
        let read_data = fs.read_file("/exact.txt").await?.unwrap();
        assert_eq!(read_data.len(), chunk_size);
        assert_eq!(read_data, data);

        // Verify only 1 chunk was created
        let ino = fs.resolve_path("/exact.txt").await?.unwrap();
        let chunk_count = fs.get_chunk_count(ino).await?;
        assert_eq!(chunk_count, 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_file_one_byte_over_chunk_size() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        // Write chunk_size + 1 bytes
        let chunk_size = fs.chunk_size();
        let data: Vec<u8> = (0..=chunk_size).map(|i| (i % 256) as u8).collect();
        fs.write_file("/overflow.txt", &data).await?;

        // Read it back
        let read_data = fs.read_file("/overflow.txt").await?.unwrap();
        assert_eq!(read_data.len(), chunk_size + 1);
        assert_eq!(read_data, data);

        // Verify 2 chunks were created
        let ino = fs.resolve_path("/overflow.txt").await?.unwrap();
        let chunk_count = fs.get_chunk_count(ino).await?;
        assert_eq!(chunk_count, 2);

        Ok(())
    }

    #[tokio::test]
    async fn test_file_spanning_multiple_chunks() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        // Write ~2.5 chunks worth of data
        let chunk_size = fs.chunk_size();
        let data_size = chunk_size * 2 + chunk_size / 2;
        let data: Vec<u8> = (0..data_size).map(|i| (i % 256) as u8).collect();
        fs.write_file("/multi.txt", &data).await?;

        // Read it back
        let read_data = fs.read_file("/multi.txt").await?.unwrap();
        assert_eq!(read_data.len(), data_size);
        assert_eq!(read_data, data);

        // Verify 3 chunks were created
        let ino = fs.resolve_path("/multi.txt").await?.unwrap();
        let chunk_count = fs.get_chunk_count(ino).await?;
        assert_eq!(chunk_count, 3);

        Ok(())
    }

    // ==================== Data Integrity Tests ====================

    #[tokio::test]
    async fn test_roundtrip_byte_for_byte() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        // Create data that spans chunk boundaries with identifiable patterns
        let chunk_size = fs.chunk_size();
        let data_size = chunk_size * 3 + 123; // Odd size spanning 4 chunks

        let data: Vec<u8> = (0..data_size).map(|i| (i % 256) as u8).collect();
        fs.write_file("/roundtrip.bin", &data).await?;

        let read_data = fs.read_file("/roundtrip.bin").await?.unwrap();
        assert_eq!(read_data.len(), data_size);
        assert_eq!(read_data, data, "Data mismatch after roundtrip");

        Ok(())
    }

    #[tokio::test]
    async fn test_binary_data_with_null_bytes() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        let chunk_size = fs.chunk_size();
        // Create data with null bytes at chunk boundaries
        let mut data = vec![0u8; chunk_size * 2 + 100];
        // Put nulls at the chunk boundary
        data[chunk_size - 1] = 0;
        data[chunk_size] = 0;
        data[chunk_size + 1] = 0;
        // Put some non-null bytes around
        data[chunk_size - 2] = 0xFF;
        data[chunk_size + 2] = 0xFF;

        fs.write_file("/nulls.bin", &data).await?;
        let read_data = fs.read_file("/nulls.bin").await?.unwrap();

        assert_eq!(read_data, data, "Null bytes at chunk boundary corrupted");

        Ok(())
    }

    #[tokio::test]
    async fn test_chunk_ordering() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        let chunk_size = fs.chunk_size();
        // Create sequential bytes spanning multiple chunks
        let data_size = chunk_size * 5;
        let data: Vec<u8> = (0..data_size).map(|i| (i % 256) as u8).collect();
        fs.write_file("/sequential.bin", &data).await?;

        let read_data = fs.read_file("/sequential.bin").await?.unwrap();

        // Verify every byte is in the correct position
        for (i, (&expected, &actual)) in data.iter().zip(read_data.iter()).enumerate() {
            assert_eq!(
                expected, actual,
                "Byte mismatch at position {}: expected {}, got {}",
                i, expected, actual
            );
        }

        Ok(())
    }

    // ==================== Edge Case Tests ====================

    #[tokio::test]
    async fn test_empty_file() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        // Write empty file
        fs.write_file("/empty.txt", &[]).await?;

        // Read it back
        let read_data = fs.read_file("/empty.txt").await?.unwrap();
        assert!(read_data.is_empty());

        // Verify 0 chunks were created
        let ino = fs.resolve_path("/empty.txt").await?.unwrap();
        let chunk_count = fs.get_chunk_count(ino).await?;
        assert_eq!(chunk_count, 0);

        // Verify size is 0
        let stats = fs.stat("/empty.txt").await?.unwrap();
        assert_eq!(stats.size, 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_overwrite_existing_file() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        let chunk_size = fs.chunk_size();

        // Write initial large file (3 chunks)
        let initial_data: Vec<u8> = (0..chunk_size * 3).map(|i| (i % 256) as u8).collect();
        fs.write_file("/overwrite.txt", &initial_data).await?;

        let ino = fs.resolve_path("/overwrite.txt").await?.unwrap();
        let initial_chunk_count = fs.get_chunk_count(ino).await?;
        assert_eq!(initial_chunk_count, 3);

        // Overwrite with smaller file (1 chunk)
        let new_data = vec![42u8; 100];
        fs.write_file("/overwrite.txt", &new_data).await?;

        // Verify old chunks are gone and new data is correct
        let read_data = fs.read_file("/overwrite.txt").await?.unwrap();
        assert_eq!(read_data, new_data);

        let new_chunk_count = fs.get_chunk_count(ino).await?;
        assert_eq!(new_chunk_count, 1);

        // Verify size is updated
        let stats = fs.stat("/overwrite.txt").await?.unwrap();
        assert_eq!(stats.size, 100);

        Ok(())
    }

    #[tokio::test]
    async fn test_overwrite_with_larger_file() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        let chunk_size = fs.chunk_size();

        // Write initial small file (1 chunk)
        let initial_data = vec![1u8; 100];
        fs.write_file("/grow.txt", &initial_data).await?;

        let ino = fs.resolve_path("/grow.txt").await?.unwrap();
        assert_eq!(fs.get_chunk_count(ino).await?, 1);

        // Overwrite with larger file (3 chunks)
        let new_data: Vec<u8> = (0..chunk_size * 3).map(|i| (i % 256) as u8).collect();
        fs.write_file("/grow.txt", &new_data).await?;

        // Verify data is correct
        let read_data = fs.read_file("/grow.txt").await?.unwrap();
        assert_eq!(read_data, new_data);
        assert_eq!(fs.get_chunk_count(ino).await?, 3);

        Ok(())
    }

    #[tokio::test]
    async fn test_very_large_file() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        // Write 1MB file
        let data_size = 1024 * 1024;
        let data: Vec<u8> = (0..data_size).map(|i| (i % 256) as u8).collect();
        fs.write_file("/large.bin", &data).await?;

        let read_data = fs.read_file("/large.bin").await?.unwrap();
        assert_eq!(read_data.len(), data_size);
        assert_eq!(read_data, data);

        // Verify correct number of chunks
        let chunk_size = fs.chunk_size();
        let expected_chunks = (data_size + chunk_size - 1) / chunk_size;
        let ino = fs.resolve_path("/large.bin").await?.unwrap();
        let actual_chunks = fs.get_chunk_count(ino).await? as usize;
        assert_eq!(actual_chunks, expected_chunks);

        Ok(())
    }

    // ==================== Configuration Tests ====================

    #[tokio::test]
    async fn test_default_chunk_size() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        assert_eq!(fs.chunk_size(), DEFAULT_CHUNK_SIZE);
        assert_eq!(fs.chunk_size(), 4096);

        Ok(())
    }

    #[tokio::test]
    async fn test_chunk_size_accessor() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        let chunk_size = fs.chunk_size();
        assert!(chunk_size > 0);

        // Write data and verify chunks match expected based on chunk_size
        let data = vec![0u8; chunk_size * 2 + 1];
        fs.write_file("/test.bin", &data).await?;

        let ino = fs.resolve_path("/test.bin").await?.unwrap();
        let chunk_count = fs.get_chunk_count(ino).await?;
        assert_eq!(chunk_count, 3);

        Ok(())
    }

    #[tokio::test]
    async fn test_config_persistence() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        // Query fs_config table directly
        let mut rows = fs
            .conn
            .query("SELECT value FROM fs_config WHERE key = 'chunk_size'", ())
            .await?;

        let row = rows.next().await?.expect("chunk_size config should exist");
        let value = row
            .get_value(0)
            .ok()
            .and_then(|v| match v {
                Value::Text(s) => Some(s.clone()),
                _ => None,
            })
            .expect("chunk_size should be a text value");

        assert_eq!(value, "4096");

        Ok(())
    }

    // ==================== Schema Tests ====================

    #[tokio::test]
    async fn test_chunk_index_uniqueness() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        // Write a file to create chunks
        let chunk_size = fs.chunk_size();
        let data = vec![0u8; chunk_size * 2];
        fs.write_file("/unique.txt", &data).await?;

        let ino = fs.resolve_path("/unique.txt").await?.unwrap();

        // Try to insert a duplicate chunk - should fail due to PRIMARY KEY constraint
        let result = fs
            .conn
            .execute(
                "INSERT INTO fs_data (ino, chunk_index, data) VALUES (?, 0, ?)",
                (ino, vec![1u8; 10]),
            )
            .await;

        assert!(result.is_err(), "Duplicate chunk_index should be rejected");

        Ok(())
    }

    #[tokio::test]
    async fn test_chunk_ordering_in_database() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        let chunk_size = fs.chunk_size();
        // Create 5 chunks with identifiable data
        let data_size = chunk_size * 5;
        let data: Vec<u8> = (0..data_size).map(|i| (i % 256) as u8).collect();
        fs.write_file("/ordered.bin", &data).await?;

        let ino = fs.resolve_path("/ordered.bin").await?.unwrap();

        // Query chunks in order
        let mut rows = fs
            .conn
            .query(
                "SELECT chunk_index FROM fs_data WHERE ino = ? ORDER BY chunk_index",
                (ino,),
            )
            .await?;

        let mut indices = Vec::new();
        while let Some(row) = rows.next().await? {
            let idx = row
                .get_value(0)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(-1);
            indices.push(idx);
        }

        assert_eq!(indices, vec![0, 1, 2, 3, 4]);

        Ok(())
    }

    // ==================== Cleanup Tests ====================

    #[tokio::test]
    async fn test_delete_file_removes_all_chunks() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        let chunk_size = fs.chunk_size();
        // Create multi-chunk file
        let data = vec![0u8; chunk_size * 4];
        fs.write_file("/deleteme.txt", &data).await?;

        let ino = fs.resolve_path("/deleteme.txt").await?.unwrap();
        assert_eq!(fs.get_chunk_count(ino).await?, 4);

        // Delete the file
        fs.remove("/deleteme.txt").await?;

        // Verify all chunks are gone
        let mut rows = fs
            .conn
            .query("SELECT COUNT(*) FROM fs_data WHERE ino = ?", (ino,))
            .await?;

        let count = rows
            .next()
            .await?
            .and_then(|r| r.get_value(0).ok().and_then(|v| v.as_integer().copied()))
            .unwrap_or(-1);

        assert_eq!(count, 0, "All chunks should be deleted");

        Ok(())
    }

    #[tokio::test]
    async fn test_multiple_files_different_sizes() -> Result<()> {
        let (fs, _dir) = create_test_fs().await?;

        let chunk_size = fs.chunk_size();

        // Create files of various sizes
        let files = vec![
            ("/tiny.txt", 10),
            ("/small.txt", chunk_size / 2),
            ("/exact.txt", chunk_size),
            ("/medium.txt", chunk_size * 2 + 100),
            ("/large.txt", chunk_size * 5),
        ];

        for (path, size) in &files {
            let data: Vec<u8> = (0..*size).map(|i| (i % 256) as u8).collect();
            fs.write_file(path, &data).await?;
        }

        // Verify each file has correct data and chunk count
        for (path, size) in &files {
            let read_data = fs.read_file(path).await?.unwrap();
            assert_eq!(read_data.len(), *size, "Size mismatch for {}", path);

            let expected_data: Vec<u8> = (0..*size).map(|i| (i % 256) as u8).collect();
            assert_eq!(read_data, expected_data, "Data mismatch for {}", path);

            let expected_chunks = if *size == 0 {
                0
            } else {
                (size + chunk_size - 1) / chunk_size
            };
            let ino = fs.resolve_path(path).await?.unwrap();
            let actual_chunks = fs.get_chunk_count(ino).await? as usize;
            assert_eq!(
                actual_chunks, expected_chunks,
                "Chunk count mismatch for {}",
                path
            );
        }

        Ok(())
    }
}
