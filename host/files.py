#!/usr/bin/python3
"""File operations after permanently dropping to the configured workspace identity.

Linux openat2 enforces a pinned project directory, no mount crossings or symlinks.
No caller supplies a Unix identity or an absolute host path.
"""
import ctypes
import errno
import json
import os
import stat

MAX_FILE = 2 * 1024 * 1024
MAX_FILES = 20000


class OpenHow(ctypes.Structure):
    _fields_ = [("flags", ctypes.c_uint64), ("mode", ctypes.c_uint64),
                ("resolve", ctypes.c_uint64)]


def relative(path, allow_root=False):
    if allow_root and path in ("", "."):
        return "."
    if not isinstance(path, str) or len(path) > 4096 or "\x00" in path:
        raise ValueError("Invalid relative file path")
    if path.startswith("/") or any(x in ("", ".", "..") for x in path.split("/")):
        raise ValueError("Paths must be relative; empty components and traversal are forbidden")
    return path


def beneath(rootfd, path, flags, mode=0, no_mounts=True):
    # openat2 is syscall 437 on supported Ubuntu x86_64 and aarch64.
    how = OpenHow(flags | os.O_CLOEXEC | os.O_NOFOLLOW, mode, 0x08 | 0x04 | (0x01 if no_mounts else 0))
    libc = ctypes.CDLL(None, use_errno=True)
    fd = libc.syscall(437, rootfd, os.fsencode(path), ctypes.byref(how), ctypes.sizeof(how))
    if fd < 0:
        code = ctypes.get_errno()
        if code == errno.ENOSYS:
            raise RuntimeError("Native files require Linux openat2 (kernel 5.6 or newer)")
        raise OSError(code, os.strerror(code), path)
    return fd


def open_root(path):
    # Also reject symlinks in operator-configured absolute paths.
    base = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        return beneath(base, path.lstrip("/"), os.O_RDONLY | os.O_DIRECTORY, no_mounts=False)
    finally:
        os.close(base)


def regular(fd):
    info = os.fstat(fd)
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise ValueError("Only regular files with one hard link are supported")
    if info.st_size > MAX_FILE:
        raise ValueError("File exceeds 2 MiB editor limit")


def operate(rootfd, request):
    operation = request["operation"]
    if operation == "list-files":
        result = []
        def walk(path, depth):
            if depth > 32:
                return
            fd = beneath(rootfd, path or ".", os.O_RDONLY | os.O_DIRECTORY)
            try:
                for name in sorted(os.listdir(fd)):
                    if name in (".git", "node_modules", "_build", "deps", "target", ".cache", ".venv", "venv", ".next", ".yarn"):
                        continue
                    item = f"{path}/{name}" if path else name
                    info = os.stat(name, dir_fd=fd, follow_symlinks=False)
                    if stat.S_ISDIR(info.st_mode):
                        walk(item, depth + 1)
                    elif stat.S_ISREG(info.st_mode) and info.st_nlink == 1:
                        result.append(item)
                        if len(result) > MAX_FILES:
                            raise ValueError("Project exceeds 20000 editor files")
            finally:
                os.close(fd)
        walk("", 0)
        return {"files": result}
    path = relative(request.get("path"))
    if operation == "read-file":
        fd = beneath(rootfd, path, os.O_RDONLY | os.O_NONBLOCK)
        try:
            regular(fd)
            with os.fdopen(fd, "rb", closefd=False) as stream:
                data = stream.read(MAX_FILE + 1)
            if len(data) > MAX_FILE or b"\x00" in data:
                raise ValueError("Editor supports UTF-8 text up to 2 MiB")
            return {"contents": data.decode("utf-8")}
        finally:
            os.close(fd)
    if operation == "write-file":
        content = request.get("content")
        if not isinstance(content, str) or "\x00" in content:
            raise ValueError("Editor content must be UTF-8 text")
        data = content.encode("utf-8")
        if len(data) > MAX_FILE:
            raise ValueError("File exceeds 2 MiB editor limit")
        # Open without truncation, validate inode type and links, then truncate.
        # In-place saves retain Unix permissions. A crash may interrupt a save.
        fd = beneath(rootfd, path, os.O_WRONLY | os.O_CREAT | os.O_NONBLOCK, 0o600)
        try:
            regular(fd)
            os.ftruncate(fd, 0)
            with os.fdopen(fd, "wb", closefd=False) as stream:
                stream.write(data)
                stream.flush()
                os.fsync(fd)
            return {"status": "ok"}
        finally:
            os.close(fd)
    raise ValueError("Unsupported file operation")


def delete_contents(rootfd):
    # All opens stay beneath the pinned project root; never follow a symlink.
    def remove(path):
        fd = beneath(rootfd, path, os.O_RDONLY | os.O_DIRECTORY)
        try:
            for name in os.listdir(fd):
                info = os.stat(name, dir_fd=fd, follow_symlinks=False)
                item = name if path == "." else f"{path}/{name}"
                if stat.S_ISDIR(info.st_mode):
                    remove(item)
                    os.rmdir(name, dir_fd=fd)
                else:
                    os.unlink(name, dir_fd=fd)
        finally:
            os.close(fd)
    remove(".")
    return {"status": "files_deleted"}
