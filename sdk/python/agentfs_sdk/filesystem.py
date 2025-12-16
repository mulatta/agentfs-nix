"""Filesystem implementation"""

import time
from dataclasses import dataclass
from typing import List, Optional, Union

from turso.aio import Connection

# File types for mode field
S_IFMT = 0o170000  # File type mask
S_IFREG = 0o100000  # Regular file
S_IFDIR = 0o040000  # Directory
S_IFLNK = 0o120000  # Symbolic link

# Default permissions
DEFAULT_FILE_MODE = S_IFREG | 0o644  # Regular file, rw-r--r--
DEFAULT_DIR_MODE = S_IFDIR | 0o755  # Directory, rwxr-xr-x

DEFAULT_CHUNK_SIZE = 4096


@dataclass
class Stats:
    """File/directory statistics

    Attributes:
        ino: Inode number
        mode: File mode and permissions
        nlink: Number of hard links
        uid: User ID
        gid: Group ID
        size: File size in bytes
        atime: Access time (Unix timestamp)
        mtime: Modification time (Unix timestamp)
        ctime: Change time (Unix timestamp)
    """

    ino: int
    mode: int
    nlink: int
    uid: int
    gid: int
    size: int
    atime: int
    mtime: int
    ctime: int

    def is_file(self) -> bool:
        """Check if this is a regular file"""
        return (self.mode & S_IFMT) == S_IFREG

    def is_directory(self) -> bool:
        """Check if this is a directory"""
        return (self.mode & S_IFMT) == S_IFDIR

    def is_symbolic_link(self) -> bool:
        """Check if this is a symbolic link"""
        return (self.mode & S_IFMT) == S_IFLNK


class Filesystem:
    """Virtual filesystem backed by SQLite

    Provides a POSIX-like filesystem interface with support for
    files, directories, and symbolic links.
    """

    def __init__(self, db: Connection):
        """Private constructor - use Filesystem.from_database() instead"""
        self._db = db
        self._root_ino = 1
        self._chunk_size = DEFAULT_CHUNK_SIZE

    @staticmethod
    async def from_database(db: Connection) -> "Filesystem":
        """Create a Filesystem from an existing database connection

        Args:
            db: An existing pyturso.aio Connection

        Returns:
            Fully initialized Filesystem instance
        """
        fs = Filesystem(db)
        await fs._initialize()
        return fs

    def get_chunk_size(self) -> int:
        """Get the configured chunk size"""
        return self._chunk_size

    async def _initialize(self) -> None:
        """Initialize the database schema"""
        # Create all tables
        await self._db.executescript("""
            CREATE TABLE IF NOT EXISTS fs_config (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS fs_inode (
                ino INTEGER PRIMARY KEY AUTOINCREMENT,
                mode INTEGER NOT NULL,
                uid INTEGER NOT NULL DEFAULT 0,
                gid INTEGER NOT NULL DEFAULT 0,
                size INTEGER NOT NULL DEFAULT 0,
                atime INTEGER NOT NULL,
                mtime INTEGER NOT NULL,
                ctime INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS fs_dentry (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                parent_ino INTEGER NOT NULL,
                ino INTEGER NOT NULL,
                UNIQUE(parent_ino, name)
            );

            CREATE INDEX IF NOT EXISTS idx_fs_dentry_parent
            ON fs_dentry(parent_ino, name);

            CREATE TABLE IF NOT EXISTS fs_data (
                ino INTEGER NOT NULL,
                chunk_index INTEGER NOT NULL,
                data BLOB NOT NULL,
                PRIMARY KEY (ino, chunk_index)
            );

            CREATE TABLE IF NOT EXISTS fs_symlink (
                ino INTEGER PRIMARY KEY,
                target TEXT NOT NULL
            );
        """)
        await self._db.commit()

        # Initialize config and root directory
        self._chunk_size = await self._ensure_root()

    async def _ensure_root(self) -> int:
        """Ensure config and root directory exist, returns the chunk_size"""
        # Ensure chunk_size config exists and get its value
        cursor = await self._db.execute("SELECT value FROM fs_config WHERE key = 'chunk_size'")
        config = await cursor.fetchone()

        if not config:
            await self._db.execute(
                "INSERT INTO fs_config (key, value) VALUES ('chunk_size', ?)",
                (str(DEFAULT_CHUNK_SIZE),),
            )
            await self._db.commit()
            chunk_size = DEFAULT_CHUNK_SIZE
        else:
            chunk_size = int(config[0]) if config[0] else DEFAULT_CHUNK_SIZE

        # Ensure root directory exists
        cursor = await self._db.execute("SELECT ino FROM fs_inode WHERE ino = ?", (self._root_ino,))
        root = await cursor.fetchone()

        if not root:
            now = int(time.time())
            await self._db.execute(
                """
                INSERT INTO fs_inode (ino, mode, uid, gid, size, atime, mtime, ctime)
                VALUES (?, ?, 0, 0, 0, ?, ?, ?)
                """,
                (self._root_ino, DEFAULT_DIR_MODE, now, now, now),
            )
            await self._db.commit()

        return chunk_size

    def _normalize_path(self, path: str) -> str:
        """Normalize a path"""
        # Remove trailing slashes except for root
        normalized = path.rstrip("/") or "/"
        # Ensure leading slash
        return normalized if normalized.startswith("/") else "/" + normalized

    def _split_path(self, path: str) -> List[str]:
        """Split path into components"""
        normalized = self._normalize_path(path)
        if normalized == "/":
            return []
        return [p for p in normalized.split("/") if p]

    async def _resolve_path(self, path: str) -> Optional[int]:
        """Resolve a path to an inode number"""
        normalized = self._normalize_path(path)

        # Root directory
        if normalized == "/":
            return self._root_ino

        parts = self._split_path(normalized)
        current_ino = self._root_ino

        # Traverse the path
        for name in parts:
            cursor = await self._db.execute(
                """
                SELECT ino FROM fs_dentry
                WHERE parent_ino = ? AND name = ?
                """,
                (current_ino, name),
            )
            result = await cursor.fetchone()

            if not result:
                return None

            current_ino = result[0]

        return current_ino

    async def _resolve_parent(self, path: str) -> Optional[tuple]:
        """Get parent directory inode and basename from path"""
        normalized = self._normalize_path(path)

        if normalized == "/":
            return None  # Root has no parent

        parts = self._split_path(normalized)
        name = parts[-1]
        parent_path = "/" if len(parts) == 1 else "/" + "/".join(parts[:-1])

        parent_ino = await self._resolve_path(parent_path)

        if parent_ino is None:
            return None

        return (parent_ino, name)

    async def _create_inode(self, mode: int, uid: int = 0, gid: int = 0) -> int:
        """Create an inode

        Note: We use RETURNING clause which requires explicit cursor close
        when working with CDC-enabled TursoDB connections.
        """
        now = int(time.time())
        cursor = self._db.cursor()
        try:
            await cursor.execute(
                """
                INSERT INTO fs_inode (mode, uid, gid, size, atime, mtime, ctime)
                VALUES (?, ?, ?, 0, ?, ?, ?)
                RETURNING ino
                """,
                (mode, uid, gid, now, now, now),
            )
            row = await cursor.fetchone()
            assert row is not None
            ino = row[0]
        finally:
            await cursor.close()
        # Commit after cursor is closed
        await self._db.commit()
        return ino

    async def _create_dentry(self, parent_ino: int, name: str, ino: int) -> None:
        """Create a directory entry"""
        await self._db.execute(
            """
            INSERT INTO fs_dentry (name, parent_ino, ino)
            VALUES (?, ?, ?)
            """,
            (name, parent_ino, ino),
        )
        await self._db.commit()

    async def _ensure_parent_dirs(self, path: str) -> None:
        """Ensure parent directories exist"""
        parts = self._split_path(path)

        # Remove the filename, keep only directory parts
        parts = parts[:-1]

        current_ino = self._root_ino
        current_path = ""

        for name in parts:
            current_path += "/" + name

            # Check if this directory exists
            cursor = await self._db.execute(
                """
                SELECT ino FROM fs_dentry
                WHERE parent_ino = ? AND name = ?
                """,
                (current_ino, name),
            )
            result = await cursor.fetchone()

            if not result:
                # Create directory
                dir_ino = await self._create_inode(DEFAULT_DIR_MODE)
                await self._create_dentry(current_ino, name, dir_ino)
                current_ino = dir_ino
            else:
                current_ino = result[0]

    async def _get_link_count(self, ino: int) -> int:
        """Get link count for an inode"""
        cursor = await self._db.execute("SELECT COUNT(*) FROM fs_dentry WHERE ino = ?", (ino,))
        result = await cursor.fetchone()
        return result[0] if result else 0

    async def write_file(self, path: str, content: Union[str, bytes]) -> None:
        """Write content to a file

        Args:
            path: Path to the file
            content: Content to write (string or bytes)

        Example:
            >>> await fs.write_file('/data/config.json', '{"key": "value"}')
        """
        # Ensure parent directories exist
        await self._ensure_parent_dirs(path)

        # Check if file already exists
        ino = await self._resolve_path(path)

        if ino is not None:
            # Update existing file
            await self._update_file_content(ino, content)
        else:
            # Create new file
            parent = await self._resolve_parent(path)
            if not parent:
                raise FileNotFoundError(f"ENOENT: parent directory does not exist: {path}")

            parent_ino, name = parent

            # Create inode
            file_ino = await self._create_inode(DEFAULT_FILE_MODE)

            # Create directory entry
            await self._create_dentry(parent_ino, name, file_ino)

            # Write content
            await self._update_file_content(file_ino, content)

    async def _update_file_content(self, ino: int, content: Union[str, bytes]) -> None:
        """Update file content"""
        buffer = content.encode("utf-8") if isinstance(content, str) else content
        now = int(time.time())

        # Delete existing data chunks
        await self._db.execute("DELETE FROM fs_data WHERE ino = ?", (ino,))

        # Write data in chunks
        if len(buffer) > 0:
            chunk_index = 0
            for offset in range(0, len(buffer), self._chunk_size):
                chunk = buffer[offset : min(offset + self._chunk_size, len(buffer))]
                await self._db.execute(
                    """
                    INSERT INTO fs_data (ino, chunk_index, data)
                    VALUES (?, ?, ?)
                    """,
                    (ino, chunk_index, chunk),
                )
                chunk_index += 1

        # Update inode size and mtime
        await self._db.execute(
            """
            UPDATE fs_inode
            SET size = ?, mtime = ?
            WHERE ino = ?
            """,
            (len(buffer), now, ino),
        )
        await self._db.commit()

    async def read_file(self, path: str, encoding: Optional[str] = "utf-8") -> Union[bytes, str]:
        """Read content from a file

        Args:
            path: Path to the file
            encoding: Text encoding (default: 'utf-8'). Set to None to return bytes.

        Returns:
            File content as string (if encoding specified) or bytes

        Example:
            >>> content = await fs.read_file('/data/config.json')
            >>> data = await fs.read_file('/data/image.png', encoding=None)
        """
        ino = await self._resolve_path(path)
        if ino is None:
            raise FileNotFoundError(f"ENOENT: no such file or directory, open '{path}'")

        # Get all data chunks
        cursor = await self._db.execute(
            """
            SELECT data FROM fs_data
            WHERE ino = ?
            ORDER BY chunk_index ASC
            """,
            (ino,),
        )
        rows = await cursor.fetchall()

        if not rows:
            combined = b""
        else:
            # Concatenate all chunks
            combined = b"".join(row[0] for row in rows)

        # Update atime
        now = int(time.time())
        await self._db.execute("UPDATE fs_inode SET atime = ? WHERE ino = ?", (now, ino))
        await self._db.commit()

        if encoding:
            return combined.decode(encoding)
        return combined

    async def readdir(self, path: str) -> List[str]:
        """List directory contents

        Args:
            path: Path to the directory

        Returns:
            List of entry names

        Example:
            >>> entries = await fs.readdir('/data')
            >>> for entry in entries:
            >>>     print(entry)
        """
        ino = await self._resolve_path(path)
        if ino is None:
            raise FileNotFoundError(f"ENOENT: no such file or directory, scandir '{path}'")

        # Get all directory entries
        cursor = await self._db.execute(
            """
            SELECT name FROM fs_dentry
            WHERE parent_ino = ?
            ORDER BY name ASC
            """,
            (ino,),
        )
        rows = await cursor.fetchall()

        return [row[0] for row in rows]

    async def delete_file(self, path: str) -> None:
        """Delete a file

        Args:
            path: Path to the file

        Example:
            >>> await fs.delete_file('/data/temp.txt')
        """
        ino = await self._resolve_path(path)
        if ino is None:
            raise FileNotFoundError(f"ENOENT: no such file or directory, unlink '{path}'")

        parent = await self._resolve_parent(path)
        if not parent:
            raise ValueError("Cannot delete root directory")

        parent_ino, name = parent

        # Delete the directory entry
        await self._db.execute(
            """
            DELETE FROM fs_dentry
            WHERE parent_ino = ? AND name = ?
            """,
            (parent_ino, name),
        )

        # Check if this was the last link to the inode
        link_count = await self._get_link_count(ino)
        if link_count == 0:
            # Delete the inode
            await self._db.execute("DELETE FROM fs_inode WHERE ino = ?", (ino,))

            # Delete all data chunks
            await self._db.execute("DELETE FROM fs_data WHERE ino = ?", (ino,))

        await self._db.commit()

    async def stat(self, path: str) -> Stats:
        """Get file/directory statistics

        Args:
            path: Path to the file or directory

        Returns:
            Stats object with file information

        Example:
            >>> stats = await fs.stat('/data/config.json')
            >>> print(f"Size: {stats.size} bytes")
            >>> print(f"Is file: {stats.is_file()}")
        """
        ino = await self._resolve_path(path)
        if ino is None:
            raise FileNotFoundError(f"ENOENT: no such file or directory, stat '{path}'")

        cursor = await self._db.execute(
            """
            SELECT ino, mode, uid, gid, size, atime, mtime, ctime
            FROM fs_inode
            WHERE ino = ?
            """,
            (ino,),
        )
        row = await cursor.fetchone()

        if not row:
            raise ValueError(f"Inode not found: {ino}")

        nlink = await self._get_link_count(ino)

        return Stats(
            ino=row[0],
            mode=row[1],
            nlink=nlink,
            uid=row[2],
            gid=row[3],
            size=row[4],
            atime=row[5],
            mtime=row[6],
            ctime=row[7],
        )
