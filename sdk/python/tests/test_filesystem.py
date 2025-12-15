"""Filesystem Integration Tests"""

import os
import tempfile

import pytest
from turso.aio import connect

from agentfs import Filesystem


@pytest.mark.asyncio
class TestFilesystemWriteOperations:
    """Filesystem write operations"""

    async def test_write_and_read_simple_text_file(self):
        """Should write and read a simple text file"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/test.txt", "Hello, World!")
            content = await fs.read_file("/test.txt")
            assert content == "Hello, World!"
            await db.close()

    async def test_write_files_in_subdirectories(self):
        """Should write and read files in subdirectories"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/dir/subdir/file.txt", "nested content")
            content = await fs.read_file("/dir/subdir/file.txt")
            assert content == "nested content"
            await db.close()

    async def test_overwrite_existing_file(self):
        """Should overwrite existing file"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/overwrite.txt", "original content")
            await fs.write_file("/overwrite.txt", "new content")
            content = await fs.read_file("/overwrite.txt")
            assert content == "new content"
            await db.close()

    async def test_handle_empty_file_content(self):
        """Should handle empty file content"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/empty.txt", "")
            content = await fs.read_file("/empty.txt")
            assert content == ""
            await db.close()

    async def test_handle_large_file_content(self):
        """Should handle large file content"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            large_content = "x" * 100000
            await fs.write_file("/large.txt", large_content)
            content = await fs.read_file("/large.txt")
            assert content == large_content
            await db.close()

    async def test_special_characters_in_content(self):
        """Should handle files with special characters in content"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            special_content = "Special chars: \n\t\r\"'\\"
            await fs.write_file("/special.txt", special_content)
            content = await fs.read_file("/special.txt")
            assert content == special_content
            await db.close()


@pytest.mark.asyncio
class TestFilesystemReadOperations:
    """Filesystem read operations"""

    async def test_error_reading_nonexistent_file(self):
        """Should throw error when reading non-existent file"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            with pytest.raises(FileNotFoundError):
                await fs.read_file("/non-existent.txt")
            await db.close()

    async def test_read_multiple_files(self):
        """Should read multiple different files"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/file1.txt", "content 1")
            await fs.write_file("/file2.txt", "content 2")
            await fs.write_file("/file3.txt", "content 3")

            assert await fs.read_file("/file1.txt") == "content 1"
            assert await fs.read_file("/file2.txt") == "content 2"
            assert await fs.read_file("/file3.txt") == "content 3"
            await db.close()


@pytest.mark.asyncio
class TestFilesystemDirectoryOperations:
    """Filesystem directory operations"""

    async def test_list_files_in_root(self):
        """Should list files in root directory"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/file1.txt", "content 1")
            await fs.write_file("/file2.txt", "content 2")
            await fs.write_file("/file3.txt", "content 3")

            files = await fs.readdir("/")
            assert "file1.txt" in files
            assert "file2.txt" in files
            assert "file3.txt" in files
            assert len(files) == 3
            await db.close()

    async def test_list_files_in_subdirectory(self):
        """Should list files in subdirectory"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/dir/file1.txt", "content 1")
            await fs.write_file("/dir/file2.txt", "content 2")
            await fs.write_file("/other/file3.txt", "content 3")

            files = await fs.readdir("/dir")
            assert "file1.txt" in files
            assert "file2.txt" in files
            assert "file3.txt" not in files
            assert len(files) == 2
            await db.close()

    async def test_distinguish_files_in_different_directories(self):
        """Should distinguish between files in different directories"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/dir1/file.txt", "content 1")
            await fs.write_file("/dir2/file.txt", "content 2")

            files1 = await fs.readdir("/dir1")
            files2 = await fs.readdir("/dir2")

            assert "file.txt" in files1
            assert "file.txt" in files2
            assert len(files1) == 1
            assert len(files2) == 1
            await db.close()

    async def test_list_subdirectories(self):
        """Should list subdirectories within a directory"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/parent/child1/file.txt", "content")
            await fs.write_file("/parent/child2/file.txt", "content")
            await fs.write_file("/parent/file.txt", "content")

            entries = await fs.readdir("/parent")
            assert "file.txt" in entries
            assert "child1" in entries
            assert "child2" in entries
            await db.close()

    async def test_nested_directory_structures(self):
        """Should handle nested directory structures"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/a/b/c/d/file.txt", "deep content")
            files = await fs.readdir("/a/b/c/d")
            assert "file.txt" in files
            await db.close()


@pytest.mark.asyncio
class TestFilesystemDeleteOperations:
    """Filesystem delete operations"""

    async def test_delete_existing_file(self):
        """Should delete an existing file"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/delete-me.txt", "content")
            await fs.delete_file("/delete-me.txt")
            with pytest.raises(FileNotFoundError):
                await fs.read_file("/delete-me.txt")
            await db.close()

    async def test_delete_nonexistent_file(self):
        """Should handle deleting non-existent file"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            with pytest.raises(FileNotFoundError, match="ENOENT"):
                await fs.delete_file("/non-existent.txt")
            await db.close()

    async def test_delete_and_update_directory_listing(self):
        """Should delete file and update directory listing"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/dir/file1.txt", "content 1")
            await fs.write_file("/dir/file2.txt", "content 2")

            await fs.delete_file("/dir/file1.txt")

            files = await fs.readdir("/dir")
            assert "file1.txt" not in files
            assert "file2.txt" in files
            assert len(files) == 1
            await db.close()

    async def test_recreate_deleted_file(self):
        """Should allow recreating deleted file"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/recreate.txt", "original")
            await fs.delete_file("/recreate.txt")
            await fs.write_file("/recreate.txt", "new content")
            content = await fs.read_file("/recreate.txt")
            assert content == "new content"
            await db.close()


@pytest.mark.asyncio
class TestFilesystemPathHandling:
    """Filesystem path handling"""

    async def test_paths_with_trailing_slashes(self):
        """Should handle paths with trailing slashes"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/dir/file.txt", "content")
            files1 = await fs.readdir("/dir")
            files2 = await fs.readdir("/dir/")
            assert files1 == files2
            await db.close()

    async def test_paths_with_special_characters(self):
        """Should handle paths with special characters"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            special_path = "/dir-with-dash/file_with_underscore.txt"
            await fs.write_file(special_path, "content")
            content = await fs.read_file(special_path)
            assert content == "content"
            await db.close()


@pytest.mark.asyncio
class TestFilesystemIntegrity:
    """Filesystem integrity tests"""

    async def test_maintain_file_hierarchy_integrity(self):
        """Should maintain file hierarchy integrity"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/root.txt", "root")
            await fs.write_file("/dir1/file.txt", "dir1")
            await fs.write_file("/dir2/file.txt", "dir2")
            await fs.write_file("/dir1/subdir/file.txt", "subdir")

            assert await fs.read_file("/root.txt") == "root"
            assert await fs.read_file("/dir1/file.txt") == "dir1"
            assert await fs.read_file("/dir2/file.txt") == "dir2"
            assert await fs.read_file("/dir1/subdir/file.txt") == "subdir"

            root_files = await fs.readdir("/")
            assert "root.txt" in root_files
            assert "dir1" in root_files
            assert "dir2" in root_files
            await db.close()

    async def test_multiple_files_same_name_different_directories(self):
        """Should support multiple files with same name in different directories"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/dir1/config.json", '{"version": 1}')
            await fs.write_file("/dir2/config.json", '{"version": 2}')

            assert await fs.read_file("/dir1/config.json") == '{"version": 1}'
            assert await fs.read_file("/dir2/config.json") == '{"version": 2}'
            await db.close()


@pytest.mark.asyncio
class TestFilesystemStandaloneUsage:
    """Filesystem standalone usage tests"""

    async def test_work_with_in_memory_database(self):
        """Should work with in-memory database"""
        db = connect(":memory:")
        await db
        fs = await Filesystem.from_database(db)

        await fs.write_file("/test.txt", "standalone content")
        content = await fs.read_file("/test.txt")
        assert content == "standalone content"
        await db.close()

    async def test_maintain_isolation_between_instances(self):
        """Should maintain isolation between instances"""
        db1 = connect(":memory:")
        await db1
        fs1 = await Filesystem.from_database(db1)

        db2 = connect(":memory:")
        await db2
        fs2 = await Filesystem.from_database(db2)

        await fs1.write_file("/test.txt", "fs1 content")
        await fs2.write_file("/test.txt", "fs2 content")

        assert await fs1.read_file("/test.txt") == "fs1 content"
        assert await fs2.read_file("/test.txt") == "fs2 content"

        await db1.close()
        await db2.close()


@pytest.mark.asyncio
class TestFilesystemPersistence:
    """Filesystem persistence tests"""

    async def test_persist_across_instances(self):
        """Should persist data across Filesystem instances"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/persist.txt", "persistent content")

            new_fs = await Filesystem.from_database(db)
            content = await new_fs.read_file("/persist.txt")
            assert content == "persistent content"
            await db.close()


@pytest.mark.asyncio
class TestFilesystemChunkSize:
    """Filesystem chunk size tests"""

    async def test_default_chunk_size(self):
        """Should have default chunk size of 4096"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            assert fs.get_chunk_size() == 4096
            await db.close()

    async def test_write_file_smaller_than_chunk_size(self):
        """Should write file smaller than chunk size"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            # Write a file smaller than chunk_size (100 bytes)
            data = "x" * 100
            await fs.write_file("/small.txt", data)

            # Read it back
            read_data = await fs.read_file("/small.txt")
            assert len(read_data) == 100
            assert read_data == data
            await db.close()

    async def test_write_file_exact_chunk_size(self):
        """Should write file exactly chunk size"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            chunk_size = fs.get_chunk_size()
            # Write exactly chunk_size bytes
            data = bytes(i % 256 for i in range(chunk_size))
            await fs.write_file("/exact.txt", data)

            # Read it back
            read_data = await fs.read_file("/exact.txt", encoding=None)
            assert len(read_data) == chunk_size
            await db.close()

    async def test_write_file_over_chunk_size(self):
        """Should write file one byte over chunk size"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            chunk_size = fs.get_chunk_size()
            # Write chunk_size + 1 bytes
            data = bytes(i % 256 for i in range(chunk_size + 1))
            await fs.write_file("/overflow.txt", data)

            # Read it back
            read_data = await fs.read_file("/overflow.txt", encoding=None)
            assert len(read_data) == chunk_size + 1
            await db.close()

    async def test_write_file_spanning_multiple_chunks(self):
        """Should write file spanning multiple chunks"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            chunk_size = fs.get_chunk_size()
            # Write ~2.5 chunks worth of data
            data_size = int(chunk_size * 2.5)
            data = bytes(i % 256 for i in range(data_size))
            await fs.write_file("/multi.txt", data)

            # Read it back
            read_data = await fs.read_file("/multi.txt", encoding=None)
            assert len(read_data) == data_size
            await db.close()


@pytest.mark.asyncio
class TestFilesystemDataIntegrity:
    """Filesystem data integrity tests"""

    async def test_roundtrip_data_byte_for_byte(self):
        """Should roundtrip data byte-for-byte"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            chunk_size = fs.get_chunk_size()
            # Create data that spans chunk boundaries with identifiable patterns
            data_size = chunk_size * 3 + 123  # Odd size spanning 4 chunks

            data = bytes(i % 256 for i in range(data_size))
            await fs.write_file("/roundtrip.bin", data)

            read_data = await fs.read_file("/roundtrip.bin", encoding=None)
            assert len(read_data) == data_size
            assert read_data == data
            await db.close()

    async def test_handle_binary_data_with_null_bytes(self):
        """Should handle binary data with null bytes"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            chunk_size = fs.get_chunk_size()
            # Create data with null bytes at chunk boundaries
            data = bytearray(chunk_size * 2 + 100)
            # Put nulls at the chunk boundary
            data[chunk_size - 1] = 0
            data[chunk_size] = 0
            data[chunk_size + 1] = 0
            # Put some non-null bytes around
            data[chunk_size - 2] = 0xFF
            data[chunk_size + 2] = 0xFF

            await fs.write_file("/nulls.bin", bytes(data))
            read_data = await fs.read_file("/nulls.bin", encoding=None)

            assert read_data[chunk_size - 2] == 0xFF
            assert read_data[chunk_size - 1] == 0
            assert read_data[chunk_size] == 0
            assert read_data[chunk_size + 1] == 0
            assert read_data[chunk_size + 2] == 0xFF
            await db.close()

    async def test_preserve_chunk_ordering(self):
        """Should preserve chunk ordering"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            chunk_size = fs.get_chunk_size()
            # Create sequential bytes spanning multiple chunks
            data_size = chunk_size * 5
            data = bytes(i % 256 for i in range(data_size))
            await fs.write_file("/sequential.bin", data)

            read_data = await fs.read_file("/sequential.bin", encoding=None)

            # Verify every byte is in the correct position
            for i in range(data_size):
                assert read_data[i] == i % 256
            await db.close()


@pytest.mark.asyncio
class TestFilesystemEdgeCases:
    """Filesystem edge case tests"""

    async def test_empty_file_with_zero_chunks(self):
        """Should handle empty file with zero chunks"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            # Write empty file
            await fs.write_file("/empty.txt", "")

            # Read it back
            read_data = await fs.read_file("/empty.txt")
            assert read_data == ""

            # Verify size is 0
            stats = await fs.stat("/empty.txt")
            assert stats.size == 0
            await db.close()

    async def test_overwrite_large_file_with_smaller(self):
        """Should overwrite large file with smaller file and clean up chunks"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            chunk_size = fs.get_chunk_size()

            # Write initial large file (3 chunks)
            initial_data = bytes(i % 256 for i in range(chunk_size * 3))
            await fs.write_file("/overwrite.txt", initial_data)

            # Overwrite with smaller file (1 chunk)
            new_data = "x" * 100
            await fs.write_file("/overwrite.txt", new_data)

            # Verify old chunks are gone and new data is correct
            read_data = await fs.read_file("/overwrite.txt")
            assert read_data == new_data

            # Verify size is updated
            stats = await fs.stat("/overwrite.txt")
            assert stats.size == 100
            await db.close()

    async def test_overwrite_small_file_with_larger(self):
        """Should overwrite small file with larger file"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            chunk_size = fs.get_chunk_size()

            # Write initial small file (1 chunk)
            initial_data = "x" * 100
            await fs.write_file("/grow.txt", initial_data)

            # Overwrite with larger file (3 chunks)
            new_data = bytes(i % 256 for i in range(chunk_size * 3))
            await fs.write_file("/grow.txt", new_data)

            # Verify data is correct
            read_data = await fs.read_file("/grow.txt", encoding=None)
            assert len(read_data) == chunk_size * 3
            await db.close()

    async def test_very_large_file(self):
        """Should handle very large file (1MB)"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            # Write 1MB file
            data_size = 1024 * 1024
            data = bytes(i % 256 for i in range(data_size))
            await fs.write_file("/large.bin", data)

            read_data = await fs.read_file("/large.bin", encoding=None)
            assert len(read_data) == data_size
            await db.close()


@pytest.mark.asyncio
class TestFilesystemStats:
    """Filesystem stats tests"""

    async def test_stat_file(self):
        """Should get file statistics"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            content = "Hello, World!"
            await fs.write_file("/test.txt", content)

            stats = await fs.stat("/test.txt")
            assert stats.is_file()
            assert not stats.is_directory()
            assert stats.size == len(content)
            assert stats.ino > 0
            await db.close()

    async def test_stat_directory(self):
        """Should get directory statistics"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            await fs.write_file("/dir/file.txt", "content")

            stats = await fs.stat("/dir")
            assert stats.is_directory()
            assert not stats.is_file()
            await db.close()

    async def test_stat_nonexistent_path(self):
        """Should throw error for non-existent path"""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = os.path.join(tmpdir, "test.db")
            db = connect(db_path)
            await db
            fs = await Filesystem.from_database(db)

            with pytest.raises(FileNotFoundError, match="ENOENT"):
                await fs.stat("/nonexistent")
            await db.close()
