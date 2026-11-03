"""Installed-filter checks, invoked only after the Rust test installs seccomp."""
import ctypes
import errno
import os
import resource
import sys

path = sys.argv[1]
parent = int(sys.argv[2])
control = len(sys.argv) > 3 and sys.argv[3] == "control"


# Linux UAPI struct file_attr (FILE_ATTR_SIZE_VER0 = 24 bytes), shared by file_getattr and
# file_setattr on all supported ABIs. A correctly sized buffer keeps a missing denial from
# failing with EINVAL and passing by accident.
class FileAttr(ctypes.Structure):
    _fields_ = [
        ("fa_xflags", ctypes.c_uint64),
        ("fa_extsize", ctypes.c_uint32),
        ("fa_nextents", ctypes.c_uint32),
        ("fa_projid", ctypes.c_uint32),
        ("fa_cowextsize", ctypes.c_uint32),
    ]


assert ctypes.sizeof(FileAttr) == 24

if control:
    # Negative control without the installed filter: the probe must be well-formed, so that
    # EPERM under the filter comes from seccomp. The verdict is reported, not asserted,
    # because the kernel version and filesystem decide it (ENOSYS before 6.17).
    libc = ctypes.CDLL(None, use_errno=True)
    for number in (468, 469):
        attr = FileAttr()
        result = libc.syscall(
            number, -100, os.fsencode(path), ctypes.byref(attr), ctypes.c_size_t(24), 0
        )
        # file_setattr with an all-zero struct is a no-op request when it is allowed at all.
        print(f"control {number}: result={result} errno={ctypes.get_errno()}")
    sys.exit(0)
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


# Linux UAPI xattr_args, shared by setxattrat and getxattrat on all supported ABIs.
class XattrArgs(ctypes.Structure):
    _fields_ = [
        ("value", ctypes.c_uint64),
        ("size", ctypes.c_uint32),
        ("flags", ctypes.c_uint32),
    ]


value = ctypes.create_string_buffer(b"probe")
xattr_args = XattrArgs(ctypes.addressof(value), 5, 0)
for number, arguments in [
    (463, (b"user.isolation-test", ctypes.byref(xattr_args), ctypes.c_size_t(16))),
    (464, (b"user.isolation-test", ctypes.byref(xattr_args), ctypes.c_size_t(16))),
    (465, (value, ctypes.c_size_t(5))),
    (466, (b"user.isolation-test",)),
]:
    ctypes.set_errno(0)
    assert libc.syscall(number, -100, os.fsencode(path), 0, *arguments) == -1, number
    assert ctypes.get_errno() == errno.EPERM, (number, ctypes.get_errno())

# file_getattr (468) and file_setattr (469): (dfd, path, struct file_attr *, usize, at_flags).
# at_flags is the fifth argument, after usize, unlike the xattrat calls above.
for number in (468, 469):
    attr = FileAttr()
    ctypes.set_errno(0)
    assert (
        libc.syscall(
            number, -100, os.fsencode(path), ctypes.byref(attr), ctypes.c_size_t(24), 0
        )
        == -1
    ), number
    assert ctypes.get_errno() == errno.EPERM, (number, ctypes.get_errno())

# Unconditional message-send denial must win even before fd/payload validation.
import platform

for number in ((46, 307) if platform.machine() == "x86_64" else (211, 269)):
    assert libc.syscall(number, -1, None, 0, 0) == -1
    assert ctypes.get_errno() == errno.EPERM
