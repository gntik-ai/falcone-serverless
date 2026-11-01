//! Real syscalls issued from a child that has the compiled seccomp filter installed.
//! The crate forbids unsafe code, so there is no fork: the test binary re-executes
//! itself and runs `confined_child` (ignored in normal runs) as the confined process.
use falcone_serverless_compiler::isolate::SeccompPlan;
use rustix::fs::{AtFlags, CWD, Mode, Timespec, Timestamps, XattrFlags};
use rustix::io::Errno;
use rustix::net::{
    AddressFamily, RecvFlags, SendAncillaryBuffer, SendFlags, SocketAddrUnix, SocketFlags,
    SocketType,
};
use rustix::process::{Resource, Rlimit};
use std::collections::BTreeMap;
use std::fs::{self, File, Permissions};
use std::io::{self, IoSlice, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::Command;

const EPERM: i32 = 1;
const ENOTSUP: i32 = 95;
const DIR_VARIABLE: &str = "ISOLATE_CONFINED_DIR";
const LIMIT_VARIABLE: &str = "ISOLATE_PARENT_NOFILE";
const WORKSPACE_MODE: u32 = 0o750;

fn outcome(result: io::Result<()>) -> String {
    match result {
        Ok(()) => "ok".into(),
        Err(error) => format!("errno{}", error.raw_os_error().unwrap_or(-1)),
    }
}
fn errno(result: Result<impl Sized, Errno>) -> io::Result<()> {
    result.map(drop).map_err(io::Error::from)
}
fn socket(kind: SocketType) -> OwnedFd {
    rustix::net::socket_with(AddressFamily::UNIX, kind, SocketFlags::CLOEXEC, None).unwrap()
}

fn datagram_pair() -> (OwnedFd, OwnedFd) {
    rustix::net::socketpair(
        AddressFamily::UNIX,
        SocketType::DGRAM,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap()
}

/// Everything created before the filter: `socket` itself is denied afterwards.
struct Fixtures {
    dir: PathBuf,
    parent: PathBuf,
    parent_limit: Rlimit,
    stream: [OwnedFd; 2],
    dgram: [OwnedFd; 2],
    sendto: [OwnedFd; 2],
    bind: [OwnedFd; 2],
    pair: (OwnedFd, OwnedFd),
    message_pair: (OwnedFd, OwnedFd),
    stream_pair: (UnixStream, UnixStream),
}
impl Fixtures {
    fn new(dir: PathBuf) -> Self {
        let limits = std::env::var(LIMIT_VARIABLE).unwrap();
        let (soft, hard) = limits.split_once(',').unwrap();
        Self {
            parent: dir.join("parent"),
            parent_limit: Rlimit {
                current: Some(soft.parse().unwrap()),
                maximum: Some(hard.parse().unwrap()),
            },
            dir,
            stream: [socket(SocketType::STREAM), socket(SocketType::STREAM)],
            dgram: [socket(SocketType::DGRAM), socket(SocketType::DGRAM)],
            sendto: [socket(SocketType::DGRAM), socket(SocketType::DGRAM)],
            bind: [socket(SocketType::DGRAM), socket(SocketType::DGRAM)],
            pair: datagram_pair(),
            message_pair: datagram_pair(),
            stream_pair: UnixStream::pair().unwrap(),
        }
    }
    fn address(&self, name: &str) -> SocketAddrUnix {
        SocketAddrUnix::new(self.dir.join(name)).unwrap()
    }
    /// Each operation is a no-op when permitted, so the "before" run is a control that
    /// proves the call reaches the kernel and the "after" run can only fail by the filter.
    fn denied(&self, phase: usize) -> Vec<(&'static str, io::Result<()>)> {
        let parent = self.parent.as_path();
        let (uid, gid) = {
            let meta = fs::metadata(parent).unwrap();
            (meta.uid(), meta.gid())
        };
        let mode = Mode::from_raw_mode(WORKSPACE_MODE);
        let untouched = fs::metadata(parent).unwrap();
        let now = Timespec {
            tv_sec: untouched.mtime(),
            tv_nsec: untouched.mtime_nsec(),
        };
        let times = Timestamps {
            last_access: now,
            last_modification: now,
        };
        // The after-run uses values that would visibly change the parent if it succeeded.
        let (mode_value, std_mode) = if phase == 0 {
            (mode, WORKSPACE_MODE)
        } else {
            (Mode::from_raw_mode(0), 0)
        };
        let times = if phase == 0 {
            times
        } else {
            Timestamps {
                last_access: Timespec {
                    tv_sec: 1,
                    tv_nsec: 0,
                },
                last_modification: Timespec {
                    tv_sec: 1,
                    tv_nsec: 0,
                },
            }
        };
        // Lowering only the soft limit keeps a failing run reversible by the parent.
        let mut parent_limit = self.parent_limit;
        if phase == 1 {
            parent_limit.current = parent_limit.current.map(|soft| soft - 1);
        }
        let open = File::open(parent).unwrap();
        let bind_name = format!("bound-{phase}.sock");
        let mut sent = SendAncillaryBuffer::default();
        vec![
            (
                "connect_stream",
                errno(rustix::net::connect(
                    &self.stream[phase],
                    &self.address("stream.sock"),
                )),
            ),
            (
                "connect_dgram",
                errno(rustix::net::connect(
                    &self.dgram[phase],
                    &self.address("dgram.sock"),
                )),
            ),
            (
                "sendto_pathname",
                errno(rustix::net::sendto(
                    &self.sendto[phase],
                    b"x",
                    SendFlags::empty(),
                    &self.address("dgram.sock"),
                )),
            ),
            (
                "bind_pathname",
                errno(rustix::net::bind(
                    &self.bind[phase],
                    &self.address(&bind_name),
                )),
            ),
            (
                "sendmsg_socketpair",
                errno(rustix::net::sendmsg(
                    &self.message_pair.0,
                    &[IoSlice::new(b"x")],
                    &mut sent,
                    SendFlags::empty(),
                )),
            ),
            (
                "prlimit_parent",
                errno(rustix::process::prlimit(
                    rustix::process::getppid(),
                    Resource::Nofile,
                    parent_limit,
                )),
            ),
            (
                "chmod",
                fs::set_permissions(parent, Permissions::from_mode(std_mode)),
            ),
            (
                "fchmod",
                open.set_permissions(Permissions::from_mode(std_mode)),
            ),
            (
                "fchmodat",
                errno(rustix::fs::chmodat(
                    CWD,
                    parent,
                    mode_value,
                    AtFlags::empty(),
                )),
            ),
            (
                "chown",
                std::os::unix::fs::chown(parent, Some(uid), Some(gid)),
            ),
            (
                "lchown",
                std::os::unix::fs::lchown(parent, Some(uid), Some(gid)),
            ),
            (
                "fchown",
                std::os::unix::fs::fchown(&open, Some(uid), Some(gid)),
            ),
            (
                "fchownat",
                errno(rustix::fs::chownat(
                    CWD,
                    parent,
                    Some(rustix::fs::Uid::from_raw(uid)),
                    Some(rustix::fs::Gid::from_raw(gid)),
                    AtFlags::empty(),
                )),
            ),
            (
                "setxattr",
                errno(rustix::fs::setxattr(
                    parent,
                    "user.escape",
                    b"1",
                    XattrFlags::empty(),
                )),
            ),
            (
                "removexattr",
                errno(rustix::fs::removexattr(parent, "user.escape")),
            ),
            (
                "utimensat",
                errno(rustix::fs::utimensat(CWD, parent, &times, AtFlags::empty())),
            ),
            (
                "futimens",
                open.set_times(
                    std::fs::FileTimes::new()
                        .set_modified(
                            std::time::UNIX_EPOCH
                                + std::time::Duration::from_secs(if phase == 0 { 0 } else { 1 }),
                        )
                        .set_accessed(std::time::UNIX_EPOCH),
                )
                .map(|_| ()),
            ),
        ]
    }
    /// Unnamed-socketpair traffic and self-referring calls that must keep working.
    fn permitted(&mut self) -> Vec<(&'static str, io::Result<()>)> {
        let mut buffer = [0u8; 8];
        let mut results = Vec::new();
        let (a, b) = &self.pair;
        results.push((
            "send_dgram_pair",
            errno(rustix::net::send(a, b"ping", SendFlags::empty()))
                .and_then(|()| errno(rustix::net::recv(b, &mut buffer, RecvFlags::empty())))
                .and_then(|()| {
                    if &buffer[..4] == b"ping" {
                        Ok(())
                    } else {
                        Err(io::ErrorKind::InvalidData.into())
                    }
                }),
        ));
        let (left, right) = &mut self.stream_pair;
        results.push((
            "write_stream_pair",
            left.write_all(b"pong").and_then(|()| {
                let mut got = [0u8; 4];
                right.read_exact(&mut got)?;
                if &got == b"pong" {
                    Ok(())
                } else {
                    Err(io::ErrorKind::InvalidData.into())
                }
            }),
        ));
        let own = rustix::process::getrlimit(Resource::Nofile);
        results.push((
            "prlimit_self",
            errno(rustix::process::prlimit(None, Resource::Nofile, own)),
        ));
        results
    }
}

#[test]
#[ignore = "re-executed by the parent tests as the confined child"]
fn confined_child() {
    let dir = PathBuf::from(std::env::var_os(DIR_VARIABLE).expect("run through confined_run"));
    let mut fixtures = Fixtures::new(dir);
    for (name, result) in fixtures.denied(0) {
        println!("CASE before:{name}={}", outcome(result));
    }
    for program in SeccompPlan::for_host().unwrap().compile().unwrap() {
        seccompiler::apply_filter(&program).unwrap();
    }
    for (name, result) in fixtures.denied(1) {
        println!("CASE after:{name}={}", outcome(result));
    }
    for (name, result) in fixtures.permitted() {
        println!("CASE after:{name}={}", outcome(result));
    }
}

struct Restore(PathBuf);
impl Drop for Restore {
    fn drop(&mut self) {
        // A broken filter may leave the parent unreadable; never leak an undeletable tree.
        let _ = fs::set_permissions(&self.0, Permissions::from_mode(0o700));
        let _ = fs::set_permissions(self.0.join("parent"), Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run_confined() -> (BTreeMap<String, String>, PathBuf, Restore) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "compiler-confined-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::create_dir(&dir).unwrap();
    let guard = Restore(dir.clone());
    fs::create_dir(dir.join("parent")).unwrap();
    fs::set_permissions(dir.join("parent"), Permissions::from_mode(WORKSPACE_MODE)).unwrap();
    let _stream = UnixListener::bind(dir.join("stream.sock")).unwrap();
    let _dgram = UnixDatagram::bind(dir.join("dgram.sock")).unwrap();
    let limit = rustix::process::getrlimit(Resource::Nofile);
    let _serialized = super::support::SPAWN_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "confined::confined_child",
            "--ignored",
            "--nocapture",
        ])
        .args(["--test-threads=1"])
        .env(DIR_VARIABLE, &dir)
        .env(
            LIMIT_VARIABLE,
            format!("{},{}", limit.current.unwrap(), limit.maximum.unwrap()),
        )
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "confined child failed:\n{text}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases = text
        .lines()
        // With --test-threads=1 libtest prefixes the first line with the test name.
        .filter_map(|line| line.split_once("CASE ").map(|(_, case)| case))
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    let after = rustix::process::getrlimit(Resource::Nofile);
    // Restore before asserting so a broken filter cannot degrade the rest of the run.
    let _ = rustix::process::prlimit(None, Resource::Nofile, limit);
    assert_eq!(after, limit, "the parent's RLIMIT_NOFILE changed");
    (cases, dir, guard)
}

#[test]
fn pathname_sockets_and_foreign_prlimit_fail_with_eperm_while_socketpair_traffic_works() {
    let (cases, dir, _guard) = run_confined();
    for name in [
        "connect_stream",
        "connect_dgram",
        "sendto_pathname",
        "bind_pathname",
        "sendmsg_socketpair",
        "prlimit_parent",
    ] {
        assert_eq!(cases[&format!("before:{name}")], "ok", "control {name}");
        assert_eq!(
            cases[&format!("after:{name}")],
            format!("errno{EPERM}"),
            "{name}"
        );
    }
    assert!(
        dir.join("bound-0.sock").exists(),
        "control bind reached the kernel"
    );
    assert!(!dir.join("bound-1.sock").exists());
    for name in ["send_dgram_pair", "write_stream_pair", "prlimit_self"] {
        assert_eq!(cases[&format!("after:{name}")], "ok", "{name}");
    }
}

#[test]
fn metadata_mutators_on_the_workspace_parent_fail_and_leave_it_unchanged() {
    let (cases, dir, _guard) = run_confined();
    let parent = dir.join("parent");
    for name in [
        "chmod",
        "fchmod",
        "fchmodat",
        "chown",
        "lchown",
        "fchown",
        "fchownat",
        "setxattr",
        "removexattr",
        "utimensat",
        "futimens",
    ] {
        let control = &cases[&format!("before:{name}")];
        // A file system without user xattrs cannot run the control, but the filter still
        // answers before the file system is consulted.
        let accepted = [
            String::from("ok"),
            format!("errno{ENOTSUP}"),
            "errno61".into(),
        ];
        assert!(
            accepted.contains(control) && (control == "ok" || name.ends_with("xattr")),
            "control {name}: {control}"
        );
        assert_eq!(
            cases[&format!("after:{name}")],
            format!("errno{EPERM}"),
            "{name}"
        );
    }
    let meta = fs::metadata(&parent).unwrap();
    assert_eq!(meta.mode() & 0o7777, WORKSPACE_MODE);
    assert_ne!(meta.mtime(), 1, "utimensat changed the mtime");
    assert!(rustix::fs::getxattr(&parent, "user.escape", &mut [0u8; 4]).is_err());
}
