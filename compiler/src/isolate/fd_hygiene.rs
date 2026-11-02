//! The descriptor ownership and post-fork boundary audited in ADR 0012.
#![allow(unsafe_code)]

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;

/// Snapshot before fork; CLOEXEC descriptors created by Command disappear in the
/// trusted trampoline exec before cleanup uses this ENOSYS fallback.
pub(super) fn snapshot() -> io::Result<Vec<RawFd>> {
    std::fs::read_dir("/proc/self/fd")?
        .map(|entry| {
            entry?
                .file_name()
                .to_str()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
        })
        .collect()
}

/// Only the trusted trampoline receives the status writer. It immediately makes
/// it CLOEXEC again, before any confinement or toolchain exec.
pub(super) fn pass_status(command: &mut Command, status: &OwnedFd) {
    let fd = status.as_raw_fd();
    // SAFETY: the closure calls only async-signal-safe fcntl and error handling;
    // the borrowed writer remains open until Command::spawn has returned.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

/// Called only by the disposable trusted trampoline, before constructing any
/// owned descriptors. Never closes a descriptor in the long-lived parent.
pub(super) fn adopt_and_clean(fd: RawFd, fallback: &[RawFd]) -> io::Result<OwnedFd> {
    if fd < 3 {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    // SAFETY: fcntl neither dereferences pointers nor assumes descriptor ownership.
    // Successful F_SETFD validates that this descriptor exists and sets CLOEXEC.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is the live pipe writer inherited by this fresh trampoline;
    // no OwnedFd has adopted it yet, and this function adopts it exactly once.
    let status = unsafe { OwnedFd::from_raw_fd(fd) };
    for (first, last) in [(3, fd as u32 - 1), (fd as u32 + 1, u32::MAX)] {
        if first > last {
            continue;
        }
        // SAFETY: close_range takes integer bounds, with no pointers. The status
        // writer and stdio are excluded; no other owned descriptors exist here.
        if unsafe { libc::syscall(libc::SYS_close_range, first, last, 0u32) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ENOSYS) {
                return Err(error);
            }
            for &other in fallback {
                if other >= 3 && other != fd && (first..=last).contains(&(other as u32)) {
                    // SAFETY: this pre-fork snapshot names only disposable inherited
                    // descriptors; stdio and the sole adopted writer are excluded.
                    unsafe {
                        libc::close(other);
                    }
                }
            }
        }
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::process::CommandExt;

    #[test]
    fn cleanup_exec_helper() {
        let Some(value) = std::env::var_os("DESCRIPTOR_CLEANUP_HELPER") else {
            return;
        };
        let fd = value.to_str().unwrap().parse().unwrap();
        let fallback: Vec<RawFd> = std::env::var("DESCRIPTOR_SNAPSHOT")
            .unwrap()
            .split(',')
            .map(|s| s.parse().unwrap())
            .collect();
        if std::env::var_os("DESCRIPTOR_FORCE_FALLBACK").is_some() {
            // Exercise ENOSYS from the actual syscall, without a mocked closer.
            use seccompiler::{SeccompAction, SeccompFilter};
            let rules = [(libc::SYS_close_range, vec![])].into_iter().collect();
            let filter: seccompiler::BpfProgram = SeccompFilter::new(
                rules,
                SeccompAction::Allow,
                SeccompAction::Errno(libc::ENOSYS as u32),
                std::env::consts::ARCH.try_into().unwrap(),
            )
            .unwrap()
            .try_into()
            .unwrap();
            seccompiler::apply_filter(&filter).unwrap();
        }
        let _status = adopt_and_clean(fd, &fallback).unwrap();
        // Python excludes the transient directory-iterator fd by checking presence
        // after listdir has closed it. The only persistent fds must be stdio.
        let error = Command::new("/usr/bin/python3").arg("-c").arg(
            "import os; fds = sorted(int(n) for n in os.listdir('/proc/self/fd') if os.path.exists('/proc/self/fd/' + n)); assert fds == [0, 1, 2], fds"
        ).exec();
        panic!("exec failed: {error}");
    }

    #[test]
    fn inherited_descriptors_are_closed_and_status_disappears_on_exec() {
        for fallback in [false, true] {
            let extra = std::fs::File::open("/dev/null").unwrap();
            let inherited = rustix::io::fcntl_dupfd_cloexec(&extra, 64).unwrap();
            rustix::io::fcntl_setfd(&inherited, rustix::io::FdFlags::empty()).unwrap();
            let (reader, writer) =
                rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
            let snapshot = snapshot()
                .unwrap()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "isolate::fd_hygiene::tests::cleanup_exec_helper",
                    "--nocapture",
                ])
                .env("DESCRIPTOR_CLEANUP_HELPER", writer.as_raw_fd().to_string())
                .env("DESCRIPTOR_SNAPSHOT", snapshot);
            if fallback {
                command.env("DESCRIPTOR_FORCE_FALLBACK", "1");
            }
            pass_status(&mut command, &writer);
            let mut child = command.spawn().unwrap();
            drop(command);
            drop(writer);
            assert!(child.wait().unwrap().success());
            let mut bytes = Vec::new();
            std::fs::File::from(reader).read_to_end(&mut bytes).unwrap();
            assert!(bytes.is_empty());
            // Parent ownership is untouched by the child cleanup.
            assert!(rustix::io::fcntl_getfd(&inherited).unwrap().is_empty());
        }
    }
}
