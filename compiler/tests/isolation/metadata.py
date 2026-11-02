"""Installed-filter checks, invoked only after the Rust test installs seccomp."""
import ctypes
import errno
import os
import resource
import sys

path = sys.argv[1]
parent = int(sys.argv[2])
limits = resource.getrlimit(resource.RLIMIT_NOFILE)
assert resource.prlimit(0, resource.RLIMIT_NOFILE, limits) == limits


def denied(operation):
    try:
        operation()
    except OSError as error:
        assert error.errno == errno.EPERM, error
    else:
        raise AssertionError("unmediated operation succeeded")


denied(lambda: resource.prlimit(parent, resource.RLIMIT_NOFILE))
denied(lambda: os.chmod(path, 0))
denied(lambda: os.chown(path, os.getuid(), os.getgid()))
denied(lambda: os.setxattr(path, "user.isolation-test", b"probe"))
denied(lambda: os.utime(path, ns=(1_000_000_000, 1_000_000_000)))
libc = ctypes.CDLL(None, use_errno=True)
assert libc.syscall(452, -100, os.fsencode(path), 0, 0) == -1
assert ctypes.get_errno() == errno.EPERM
# Unconditional message-send denial must win even before fd/payload validation.
import platform

for number in ((46, 307) if platform.machine() == "x86_64" else (211, 269)):
    assert libc.syscall(number, -1, None, 0, 0) == -1
    assert ctypes.get_errno() == errno.EPERM
