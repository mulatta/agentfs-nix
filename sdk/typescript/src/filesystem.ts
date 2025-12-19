import type { DatabasePromise } from '@tursodatabase/database-common';
import { createFsError } from './errors.js';
import {
  assertInodeIsDirectory,
  assertNotRoot,
  assertNotSymlinkMode,
  assertReadableExistingInode,
  assertReaddirTargetInode,
  assertUnlinkTargetInode,
  assertWritableExistingInode,
  getInodeModeOrThrow,
  normalizeRmOptions,
  throwENOENTUnlessForce,
} from './guards.js';

// File types for mode field
export const S_IFMT = 0o170000;   // File type mask
export const S_IFREG = 0o100000;  // Regular file
export const S_IFDIR = 0o040000;  // Directory
export const S_IFLNK = 0o120000;  // Symbolic link

// Default permissions
const DEFAULT_FILE_MODE = S_IFREG | 0o644;  // Regular file, rw-r--r--
const DEFAULT_DIR_MODE = S_IFDIR | 0o755;   // Directory, rwxr-xr-x

const DEFAULT_CHUNK_SIZE = 4096;

export interface Stats {
  ino: number;
  mode: number;
  nlink: number;
  uid: number;
  gid: number;
  size: number;
  atime: number;
  mtime: number;
  ctime: number;
  isFile(): boolean;
  isDirectory(): boolean;
  isSymbolicLink(): boolean;
}

export class Filesystem {
  private db: DatabasePromise;
  private bufferCtor: BufferConstructor;
  private rootIno: number = 1;
  private chunkSize: number = DEFAULT_CHUNK_SIZE;

  private constructor(db: DatabasePromise, b: BufferConstructor) {
    this.db = db;
    this.bufferCtor = b;
  }

  /**
   * Create a Filesystem from an existing database connection
   */
  static async fromDatabase(db: DatabasePromise, b?: BufferConstructor): Promise<Filesystem> {
    const fs = new Filesystem(db, b ?? Buffer);
    await fs.initialize();
    return fs;
  }

  /**
   * Get the configured chunk size
   */
  getChunkSize(): number {
    return this.chunkSize;
  }

  private async initialize(): Promise<void> {
    // Create the config table
    await this.db.exec(`
      CREATE TABLE IF NOT EXISTS fs_config (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
      )
    `);

    // Create the inode table
    await this.db.exec(`
      CREATE TABLE IF NOT EXISTS fs_inode (
        ino INTEGER PRIMARY KEY AUTOINCREMENT,
        mode INTEGER NOT NULL,
        uid INTEGER NOT NULL DEFAULT 0,
        gid INTEGER NOT NULL DEFAULT 0,
        size INTEGER NOT NULL DEFAULT 0,
        atime INTEGER NOT NULL,
        mtime INTEGER NOT NULL,
        ctime INTEGER NOT NULL
      )
    `);

    // Create the directory entry table
    await this.db.exec(`
      CREATE TABLE IF NOT EXISTS fs_dentry (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL,
        parent_ino INTEGER NOT NULL,
        ino INTEGER NOT NULL,
        UNIQUE(parent_ino, name)
      )
    `);

    // Create index for efficient path lookups
    await this.db.exec(`
      CREATE INDEX IF NOT EXISTS idx_fs_dentry_parent
      ON fs_dentry(parent_ino, name)
    `);

    // Create the data chunks table
    await this.db.exec(`
      CREATE TABLE IF NOT EXISTS fs_data (
        ino INTEGER NOT NULL,
        chunk_index INTEGER NOT NULL,
        data BLOB NOT NULL,
        PRIMARY KEY (ino, chunk_index)
      )
    `);

    // Create the symlink table
    await this.db.exec(`
      CREATE TABLE IF NOT EXISTS fs_symlink (
        ino INTEGER PRIMARY KEY,
        target TEXT NOT NULL
      )
    `);

    // Initialize config and root directory, and get chunk_size
    this.chunkSize = await this.ensureRoot();
  }

  /**
   * Ensure config and root directory exist, returns the chunk_size
   */
  private async ensureRoot(): Promise<number> {
    // Ensure chunk_size config exists and get its value
    const configStmt = this.db.prepare("SELECT value FROM fs_config WHERE key = 'chunk_size'");
    const config = await configStmt.get() as { value: string } | undefined;

    let chunkSize: number;
    if (!config) {
      const insertConfigStmt = this.db.prepare(`
        INSERT INTO fs_config (key, value) VALUES ('chunk_size', ?)
      `);
      await insertConfigStmt.run(DEFAULT_CHUNK_SIZE.toString());
      chunkSize = DEFAULT_CHUNK_SIZE;
    } else {
      chunkSize = parseInt(config.value, 10) || DEFAULT_CHUNK_SIZE;
    }

    // Ensure root directory exists
    const stmt = this.db.prepare('SELECT ino FROM fs_inode WHERE ino = ?');
    const root = await stmt.get(this.rootIno);

    if (!root) {
      const now = Math.floor(Date.now() / 1000);
      const insertStmt = this.db.prepare(`
        INSERT INTO fs_inode (ino, mode, uid, gid, size, atime, mtime, ctime)
        VALUES (?, ?, 0, 0, 0, ?, ?, ?)
      `);
      await insertStmt.run(this.rootIno, DEFAULT_DIR_MODE, now, now, now);
    }

    return chunkSize;
  }

  /**
   * Normalize a path
   */
  private normalizePath(path: string): string {
    // Remove trailing slashes except for root
    const normalized = path.replace(/\/+$/, '') || '/';
    // Ensure leading slash
    return normalized.startsWith('/') ? normalized : '/' + normalized;
  }

  /**
   * Split path into components
   */
  private splitPath(path: string): string[] {
    const normalized = this.normalizePath(path);
    if (normalized === '/') return [];
    return normalized.split('/').filter(p => p);
  }

  private async resolvePathOrThrow(
    path: string,
    syscall: string
  ): Promise<{ normalizedPath: string; ino: number }> {
    const normalizedPath = this.normalizePath(path);
    const ino = await this.resolvePath(normalizedPath);
    if (ino === null) {
      throw createFsError({
        code: 'ENOENT',
        syscall,
        path: normalizedPath,
        message: 'no such file or directory',
      });
    }
    return { normalizedPath, ino };
  }

  /**
   * Resolve a path to an inode number
   */
  private async resolvePath(path: string): Promise<number | null> {
    const normalized = this.normalizePath(path);

    // Root directory
    if (normalized === '/') {
      return this.rootIno;
    }

    const parts = this.splitPath(normalized);
    let currentIno = this.rootIno;

    // Traverse the path
    for (const name of parts) {
      const stmt = this.db.prepare(`
        SELECT ino FROM fs_dentry
        WHERE parent_ino = ? AND name = ?
      `);
      const result = await stmt.get(currentIno, name) as { ino: number } | undefined;

      if (!result) {
        return null;
      }

      currentIno = result.ino;
    }

    return currentIno;
  }

  /**
   * Get parent directory inode and basename from path
   */
  private async resolveParent(path: string): Promise<{ parentIno: number; name: string } | null> {
    const normalized = this.normalizePath(path);

    if (normalized === '/') {
      return null; // Root has no parent
    }

    const parts = this.splitPath(normalized);
    const name = parts[parts.length - 1];
    const parentPath = parts.length === 1 ? '/' : '/' + parts.slice(0, -1).join('/');

    const parentIno = await this.resolvePath(parentPath);

    if (parentIno === null) {
      return null;
    }

    return { parentIno, name };
  }

  /**
   * Create an inode
   */
  private async createInode(mode: number, uid: number = 0, gid: number = 0): Promise<number> {
    const now = Math.floor(Date.now() / 1000);
    const stmt = this.db.prepare(`
      INSERT INTO fs_inode (mode, uid, gid, size, atime, mtime, ctime)
      VALUES (?, ?, ?, 0, ?, ?, ?)
      RETURNING ino
    `);
    const { ino } = await stmt.get(mode, uid, gid, now, now, now);
    return Number(ino);
  }

  /**
   * Create a directory entry
   */
  private async createDentry(parentIno: number, name: string, ino: number): Promise<void> {
    const stmt = this.db.prepare(`
      INSERT INTO fs_dentry (name, parent_ino, ino)
      VALUES (?, ?, ?)
    `);
    await stmt.run(name, parentIno, ino);
  }

  /**
   * Ensure parent directories exist
   */
  private async ensureParentDirs(path: string): Promise<void> {
    const parts = this.splitPath(path);

    // Remove the filename, keep only directory parts
    parts.pop();

    let currentIno = this.rootIno;

    for (const name of parts) {
      // Check if this directory exists
      const stmt = this.db.prepare(`
        SELECT ino FROM fs_dentry
        WHERE parent_ino = ? AND name = ?
      `);
      const result = await stmt.get(currentIno, name) as { ino: number } | undefined;

      if (!result) {
        // Create directory
        const dirIno = await this.createInode(DEFAULT_DIR_MODE);
        await this.createDentry(currentIno, name, dirIno);
        currentIno = dirIno;
      } else {
        // Ensure existing path component is actually a directory
        await assertInodeIsDirectory(this.db, result.ino, 'open', this.normalizePath(path));
        currentIno = result.ino;
      }
    }
  }

  /**
   * Get link count for an inode
   */
  private async getLinkCount(ino: number): Promise<number> {
    const stmt = this.db.prepare('SELECT COUNT(*) as count FROM fs_dentry WHERE ino = ?');
    const result = await stmt.get(ino) as { count: number };
    return result.count;
  }

  private async getInodeMode(ino: number): Promise<number | null> {
    const stmt = this.db.prepare('SELECT mode FROM fs_inode WHERE ino = ?');
    const row = await stmt.get(ino) as { mode: number } | undefined;
    return row?.mode ?? null;
  }

  async writeFile(
    path: string,
    content: string | Buffer,
    options?: BufferEncoding | { encoding?: BufferEncoding }
  ): Promise<void> {
    // Ensure parent directories exist
    await this.ensureParentDirs(path);

    // Check if file already exists
    const ino = await this.resolvePath(path);

    const encoding = typeof options === 'string'
      ? options
      : options?.encoding;

    const normalizedPath = this.normalizePath(path);
    if (ino !== null) {
      await assertWritableExistingInode(this.db, ino, 'open', normalizedPath);
      // Update existing file
      await this.updateFileContent(ino, content, encoding);
    } else {
      // Create new file
      const parent = await this.resolveParent(path);
      if (!parent) {
        throw createFsError({
          code: 'ENOENT',
          syscall: 'open',
          path: normalizedPath,
          message: 'no such file or directory',
        });
      }

      // Ensure parent is a directory
      await assertInodeIsDirectory(this.db, parent.parentIno, 'open', normalizedPath);

      // Create inode
      const fileIno = await this.createInode(DEFAULT_FILE_MODE);

      // Create directory entry
      await this.createDentry(parent.parentIno, parent.name, fileIno);

      // Write content
      await this.updateFileContent(fileIno, content, encoding);
    }
  }

  private async updateFileContent(
    ino: number,
    content: string | Buffer,
    encoding?: BufferEncoding
  ): Promise<void> {
    const buffer = typeof content === 'string'
      ? this.bufferCtor.from(content, encoding ?? 'utf8')
      : content;
    const now = Math.floor(Date.now() / 1000);

    // Delete existing data chunks
    const deleteStmt = this.db.prepare('DELETE FROM fs_data WHERE ino = ?');
    await deleteStmt.run(ino);

    // Write data in chunks
    if (buffer.length > 0) {
      const stmt = this.db.prepare(`
        INSERT INTO fs_data (ino, chunk_index, data)
        VALUES (?, ?, ?)
      `);

      let chunkIndex = 0;
      for (let offset = 0; offset < buffer.length; offset += this.chunkSize) {
        const chunk = buffer.subarray(offset, Math.min(offset + this.chunkSize, buffer.length));
        await stmt.run(ino, chunkIndex, chunk);
        chunkIndex++;
      }
    }

    // Update inode size and mtime
    const updateStmt = this.db.prepare(`
      UPDATE fs_inode
      SET size = ?, mtime = ?
      WHERE ino = ?
    `);
    await updateStmt.run(buffer.length, now, ino);
  }

  async readFile(
    path: string,
    options?: BufferEncoding | { encoding?: BufferEncoding }
  ): Promise<Buffer | string> {
    // Normalize options
    const encoding = typeof options === 'string'
      ? options
      : options?.encoding;

    const { normalizedPath, ino } = await this.resolvePathOrThrow(path, 'open');

    await assertReadableExistingInode(this.db, ino, 'open', normalizedPath);

    // Get all data chunks
    const stmt = this.db.prepare(`
      SELECT data FROM fs_data
      WHERE ino = ?
      ORDER BY chunk_index ASC
    `);
    const rows = await stmt.all(ino) as { data: Buffer }[];

    let combined: Buffer;
    if (rows.length === 0) {
      combined = this.bufferCtor.alloc(0);
    } else {
      // Concatenate all chunks
      const buffers = rows.map(row => row.data);
      combined = this.bufferCtor.concat(buffers);
    }

    // Update atime
    const now = Math.floor(Date.now() / 1000);
    const updateStmt = this.db.prepare('UPDATE fs_inode SET atime = ? WHERE ino = ?');
    await updateStmt.run(now, ino);

    if (encoding) {
      return combined.toString(encoding);
    }
    return combined;
  }

  async readdir(path: string): Promise<string[]> {
    const { normalizedPath, ino } = await this.resolvePathOrThrow(path, 'scandir');

    await assertReaddirTargetInode(this.db, ino, normalizedPath);

    // Get all directory entries
    const stmt = this.db.prepare(`
      SELECT name FROM fs_dentry
      WHERE parent_ino = ?
      ORDER BY name ASC
    `);
    const rows = await stmt.all(ino) as { name: string }[];

    return rows.map(row => row.name);
  }

  async unlink(path: string): Promise<void> {
    const normalizedPath = this.normalizePath(path);
    assertNotRoot(normalizedPath, 'unlink');
    const { ino } = await this.resolvePathOrThrow(normalizedPath, 'unlink');

    await assertUnlinkTargetInode(this.db, ino, normalizedPath);

    const parent = (await this.resolveParent(normalizedPath))!;
    // parent is guaranteed to exist here since normalizedPath !== '/'

    // Delete the directory entry
    const stmt = this.db.prepare(`
      DELETE FROM fs_dentry
      WHERE parent_ino = ? AND name = ?
    `);
    await stmt.run(parent.parentIno, parent.name);

    // Check if this was the last link to the inode
    const linkCount = await this.getLinkCount(ino);
    if (linkCount === 0) {
      // Delete the inode
      const deleteInodeStmt = this.db.prepare('DELETE FROM fs_inode WHERE ino = ?');
      await deleteInodeStmt.run(ino);

      // Delete all data chunks
      const deleteDataStmt = this.db.prepare('DELETE FROM fs_data WHERE ino = ?');
      await deleteDataStmt.run(ino);
    }
  }

  // Backwards-compatible alias
  async deleteFile(path: string): Promise<void> {
    return await this.unlink(path);
  }

  async stat(path: string): Promise<Stats> {
    const { normalizedPath, ino } = await this.resolvePathOrThrow(path, 'stat');

    const stmt = this.db.prepare(`
      SELECT ino, mode, uid, gid, size, atime, mtime, ctime
      FROM fs_inode
      WHERE ino = ?
    `);
    const row = await stmt.get(ino) as {
      ino: number;
      mode: number;
      uid: number;
      gid: number;
      size: number;
      atime: number;
      mtime: number;
      ctime: number;
    } | undefined;

    if (!row) {
      throw createFsError({
        code: 'ENOENT',
        syscall: 'stat',
        path: normalizedPath,
        message: 'no such file or directory',
      });
    }

    const nlink = await this.getLinkCount(ino);

    return {
      ino: row.ino,
      mode: row.mode,
      nlink: nlink,
      uid: row.uid,
      gid: row.gid,
      size: row.size,
      atime: row.atime,
      mtime: row.mtime,
      ctime: row.ctime,
      isFile: () => (row.mode & S_IFMT) === S_IFREG,
      isDirectory: () => (row.mode & S_IFMT) === S_IFDIR,
      isSymbolicLink: () => (row.mode & S_IFMT) === S_IFLNK,
    };
  }

  /**
   * Create a directory (non-recursive, no options yet)
   */
    async mkdir(path: string): Promise<void> {
      const normalizedPath = this.normalizePath(path);
  
      const existing = await this.resolvePath(normalizedPath);
      if (existing !== null) {
        throw createFsError({
          code: 'EEXIST',
          syscall: 'mkdir',
          path: normalizedPath,
          message: 'file already exists',
        });
      }
  
      const parent = await this.resolveParent(normalizedPath);
      if (!parent) {
        throw createFsError({
          code: 'ENOENT',
          syscall: 'mkdir',
          path: normalizedPath,
          message: 'no such file or directory',
        });
      }
  
      await assertInodeIsDirectory(this.db, parent.parentIno, 'mkdir', normalizedPath);
  
      const dirIno = await this.createInode(DEFAULT_DIR_MODE);
      try {
        await this.createDentry(parent.parentIno, parent.name, dirIno);
      } catch {
        throw createFsError({
          code: 'EEXIST',
          syscall: 'mkdir',
          path: normalizedPath,
          message: 'file already exists',
        });
      }
    }

  /**
   * Remove a file or directory
   */
  async rm(
    path: string,
    options?: { force?: boolean; recursive?: boolean }
  ): Promise<void> {
    const normalizedPath = this.normalizePath(path);
    const { force, recursive } = normalizeRmOptions(options);
    assertNotRoot(normalizedPath, 'rm');

    const ino = await this.resolvePath(normalizedPath);
    if (ino === null) {
      throwENOENTUnlessForce(normalizedPath, 'rm', force);
      return;
    }

    const mode = await getInodeModeOrThrow(this.db, ino, 'rm', normalizedPath);
    assertNotSymlinkMode(mode, 'rm', normalizedPath);

    const parent = await this.resolveParent(normalizedPath);
    if (!parent) {
      throw createFsError({
        code: 'EPERM',
        syscall: 'rm',
        path: normalizedPath,
        message: 'operation not permitted',
      });
    }

    if ((mode & S_IFMT) === S_IFDIR) {
      if (!recursive) {
        throw createFsError({
          code: 'EISDIR',
          syscall: 'rm',
          path: normalizedPath,
          message: 'illegal operation on a directory',
        });
      }

      await this.rmDirContentsRecursive(ino);
      await this.removeDentryAndMaybeInode(parent.parentIno, parent.name, ino);
      return;
    }

    // Regular file
    await this.removeDentryAndMaybeInode(parent.parentIno, parent.name, ino);
  }

  private async rmDirContentsRecursive(dirIno: number): Promise<void> {
    const stmt = this.db.prepare(`
      SELECT name, ino FROM fs_dentry
      WHERE parent_ino = ?
      ORDER BY name ASC
    `);
    const children = await stmt.all(dirIno) as { name: string; ino: number }[];

    for (const child of children) {
      const mode = await this.getInodeMode(child.ino);
      if (mode === null) {
        // DB inconsistency; treat as already gone
        continue;
      }

      if ((mode & S_IFMT) === S_IFDIR) {
        await this.rmDirContentsRecursive(child.ino);
        await this.removeDentryAndMaybeInode(dirIno, child.name, child.ino);
      } else {
        // Not supported yet (symlinks)
        assertNotSymlinkMode(mode, 'rm', '<symlink>');
        await this.removeDentryAndMaybeInode(dirIno, child.name, child.ino);
      }
    }
  }

  private async removeDentryAndMaybeInode(parentIno: number, name: string, ino: number): Promise<void> {
    const stmt = this.db.prepare(`
      DELETE FROM fs_dentry
      WHERE parent_ino = ? AND name = ?
    `);
    await stmt.run(parentIno, name);

    const linkCount = await this.getLinkCount(ino);
    if (linkCount === 0) {
      const deleteInodeStmt = this.db.prepare('DELETE FROM fs_inode WHERE ino = ?');
      await deleteInodeStmt.run(ino);

      const deleteDataStmt = this.db.prepare('DELETE FROM fs_data WHERE ino = ?');
      await deleteDataStmt.run(ino);
    }
  }

    /**
   * Remove an empty directory
   */
    async rmdir(path: string): Promise<void> {
      const normalizedPath = this.normalizePath(path);
      assertNotRoot(normalizedPath, 'rmdir');
  
      const { ino } = await this.resolvePathOrThrow(normalizedPath, 'rmdir');
  
      const mode = await getInodeModeOrThrow(this.db, ino, 'rmdir', normalizedPath);
      assertNotSymlinkMode(mode, 'rmdir', normalizedPath);
      if ((mode & S_IFMT) !== S_IFDIR) {
        throw createFsError({
          code: 'ENOTDIR',
          syscall: 'rmdir',
          path: normalizedPath,
          message: 'not a directory',
        });
      }
  
      const stmt = this.db.prepare(`
        SELECT 1 as one FROM fs_dentry
        WHERE parent_ino = ?
        LIMIT 1
      `);
      const child = await stmt.get(ino) as { one: number } | undefined;
      if (child) {
        throw createFsError({
          code: 'ENOTEMPTY',
          syscall: 'rmdir',
          path: normalizedPath,
          message: 'directory not empty',
        });
      }
  
      const parent = await this.resolveParent(normalizedPath);
      if (!parent) {
        throw createFsError({
          code: 'EPERM',
          syscall: 'rmdir',
          path: normalizedPath,
          message: 'operation not permitted',
        });
      }
  
      await this.removeDentryAndMaybeInode(parent.parentIno, parent.name, ino);
    }

    /**
   * Rename (move) a file or directory
   */
    async rename(oldPath: string, newPath: string): Promise<void> {
      const oldNormalized = this.normalizePath(oldPath);
      const newNormalized = this.normalizePath(newPath);

      // No-op
      if (oldNormalized === newNormalized) return;

      assertNotRoot(oldNormalized, 'rename');
      assertNotRoot(newNormalized, 'rename');

      const oldParent = await this.resolveParent(oldNormalized);
      if (!oldParent) {
        throw createFsError({
          code: 'EPERM',
          syscall: 'rename',
          path: oldNormalized,
          message: 'operation not permitted',
        });
      }

      const newParent = await this.resolveParent(newNormalized);
      if (!newParent) {
        throw createFsError({
          code: 'ENOENT',
          syscall: 'rename',
          path: newNormalized,
          message: 'no such file or directory',
        });
      }

      // Ensure destination parent exists and is a directory
      await assertInodeIsDirectory(this.db, newParent.parentIno, 'rename', newNormalized);

      await this.db.exec('BEGIN');
      try {
        const oldResolved = await this.resolvePathOrThrow(oldNormalized, 'rename');
        const oldIno = oldResolved.ino;
        const oldMode = await getInodeModeOrThrow(this.db, oldIno, 'rename', oldNormalized);
        assertNotSymlinkMode(oldMode, 'rename', oldNormalized);
        const oldIsDir = (oldMode & S_IFMT) === S_IFDIR;

        // Prevent renaming a directory into its own subtree (would create cycles).
        if (oldIsDir && newNormalized.startsWith(oldNormalized + '/')) {
          throw createFsError({
            code: 'EINVAL',
            syscall: 'rename',
            path: newNormalized,
            message: 'invalid argument',
          });
        }

        const newIno = await this.resolvePath(newNormalized);
        if (newIno !== null) {
          const newMode = await getInodeModeOrThrow(this.db, newIno, 'rename', newNormalized);
          assertNotSymlinkMode(newMode, 'rename', newNormalized);
          const newIsDir = (newMode & S_IFMT) === S_IFDIR;

          if (newIsDir && !oldIsDir) {
            throw createFsError({
              code: 'EISDIR',
              syscall: 'rename',
              path: newNormalized,
              message: 'illegal operation on a directory',
            });
          }
          if (!newIsDir && oldIsDir) {
            throw createFsError({
              code: 'ENOTDIR',
              syscall: 'rename',
              path: newNormalized,
              message: 'not a directory',
            });
          }

          // If replacing a directory, it must be empty.
          if (newIsDir) {
            const stmt = this.db.prepare(`
              SELECT 1 as one FROM fs_dentry
              WHERE parent_ino = ?
              LIMIT 1
            `);
            const child = await stmt.get(newIno) as { one: number } | undefined;
            if (child) {
              throw createFsError({
                code: 'ENOTEMPTY',
                syscall: 'rename',
                path: newNormalized,
                message: 'directory not empty',
              });
            }
          }

          // Remove the destination entry (and inode if this was the last link)
          await this.removeDentryAndMaybeInode(newParent.parentIno, newParent.name, newIno);
        }

        // Move the directory entry
        const stmt = this.db.prepare(`
          UPDATE fs_dentry
          SET parent_ino = ?, name = ?
          WHERE parent_ino = ? AND name = ?
        `);
        await stmt.run(newParent.parentIno, newParent.name, oldParent.parentIno, oldParent.name);

        // Update timestamps
        const now = Math.floor(Date.now() / 1000);
        const updateInodeCtimeStmt = this.db.prepare(`
          UPDATE fs_inode
          SET ctime = ?
          WHERE ino = ?
        `);
        await updateInodeCtimeStmt.run(now, oldIno);

        const updateDirTimesStmt = this.db.prepare(`
          UPDATE fs_inode
          SET mtime = ?, ctime = ?
          WHERE ino = ?
        `);
        await updateDirTimesStmt.run(now, now, oldParent.parentIno);
        if (newParent.parentIno !== oldParent.parentIno) {
          await updateDirTimesStmt.run(now, now, newParent.parentIno);
        }

        await this.db.exec('COMMIT');
      } catch (e) {
        await this.db.exec('ROLLBACK');
        throw e;
      }
    }

  /**
  * Copy a file. Overwrites destination if it exists.
  */
  async copyFile(src: string, dest: string): Promise<void> {
    const srcNormalized = this.normalizePath(src);
    const destNormalized = this.normalizePath(dest);

    if (srcNormalized === destNormalized) {
      throw createFsError({
        code: 'EINVAL',
        syscall: 'copyfile',
        path: destNormalized,
        message: 'invalid argument',
      });
    }

    // Resolve and validate source
    // node uses copyfile as syscall name even though it's not a syscall
    const { ino: srcIno } = await this.resolvePathOrThrow(srcNormalized, 'copyfile');
    await assertReadableExistingInode(this.db, srcIno, 'copyfile', srcNormalized);

    const stmt = this.db.prepare(`
      SELECT mode, uid, gid, size FROM fs_inode WHERE ino = ?
    `);
    const srcRow = await stmt.get(srcIno) as
      | { mode: number; uid: number; gid: number; size: number }
      | undefined;
    if (!srcRow) {
      throw createFsError({
        code: 'ENOENT',
        syscall: 'copyfile',
        path: srcNormalized,
        message: 'no such file or directory',
      });
    }

    // Destination parent must exist and be a directory (Node does not create parents)
    const destParent = await this.resolveParent(destNormalized);
    if (!destParent) {
      throw createFsError({
        code: 'ENOENT',
        syscall: 'copyfile',
        path: destNormalized,
        message: 'no such file or directory',
      });
    }
    await assertInodeIsDirectory(this.db, destParent.parentIno, 'copyfile', destNormalized);

    await this.db.exec('BEGIN');
    try {
      const now = Math.floor(Date.now() / 1000);

      // If destination exists, it must be a file (overwrite semantics).
      const destIno = await this.resolvePath(destNormalized);
      if (destIno !== null) {
        const destMode = await getInodeModeOrThrow(this.db, destIno, 'copyfile', destNormalized);
        assertNotSymlinkMode(destMode, 'copyfile', destNormalized);
        if ((destMode & S_IFMT) === S_IFDIR) {
          throw createFsError({
            code: 'EISDIR',
            syscall: 'copyfile',
            path: destNormalized,
            message: 'illegal operation on a directory',
          });
        }

        // Replace destination contents
        const deleteStmt = this.db.prepare('DELETE FROM fs_data WHERE ino = ?');
        await deleteStmt.run(destIno);

        const copyStmt = this.db.prepare(`
          INSERT INTO fs_data (ino, chunk_index, data)
          SELECT ?, chunk_index, data
          FROM fs_data
          WHERE ino = ?
          ORDER BY chunk_index ASC
        `);
        await copyStmt.run(destIno, srcIno);

        const updateStmt = this.db.prepare(`
          UPDATE fs_inode
          SET mode = ?, uid = ?, gid = ?, size = ?, mtime = ?, ctime = ?
          WHERE ino = ?
        `);
        await updateStmt.run(srcRow.mode, srcRow.uid, srcRow.gid, srcRow.size, now, now, destIno);
      } else {
        // Create new destination inode + dentry
        const destInoCreated = await this.createInode(srcRow.mode, srcRow.uid, srcRow.gid);
        await this.createDentry(destParent.parentIno, destParent.name, destInoCreated);

        const copyStmt = this.db.prepare(`
          INSERT INTO fs_data (ino, chunk_index, data)
          SELECT ?, chunk_index, data
          FROM fs_data
          WHERE ino = ?
          ORDER BY chunk_index ASC
        `);
        await copyStmt.run(destInoCreated, srcIno);

        const updateStmt = this.db.prepare(`
          UPDATE fs_inode
          SET size = ?, mtime = ?, ctime = ?
          WHERE ino = ?
        `);
        await updateStmt.run(srcRow.size, now, now, destInoCreated);
      }

      await this.db.exec('COMMIT');
    } catch (e) {
      await this.db.exec('ROLLBACK');
      throw e;
    }
  }
}
