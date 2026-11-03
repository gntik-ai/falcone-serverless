use falcone_serverless_compiler::isolate::SeccompPlan;
use seccompiler::BpfProgram;

// A test-only cBPF interpreter: exercise the actual compiled program without installing it.
fn verdict(program: &BpfProgram, nr: u32, arch: u32, args: [u64; 6]) -> u32 {
    let mut data = [0u8; 64];
    data[..4].copy_from_slice(&nr.to_le_bytes());
    data[4..8].copy_from_slice(&arch.to_le_bytes());
    for (index, arg) in args.iter().enumerate() {
        data[16 + index * 8..24 + index * 8].copy_from_slice(&arg.to_le_bytes());
    }
    let (mut pc, mut acc) = (0usize, 0u32);
    for _ in 0..10000 {
        let op = &program[pc];
        pc += 1;
        match op.code {
            0x20 => {
                acc = u32::from_le_bytes(data[op.k as usize..op.k as usize + 4].try_into().unwrap())
            }
            0x54 => acc &= op.k,
            0x15 => pc += if acc == op.k { op.jt } else { op.jf } as usize,
            0x25 => pc += if acc > op.k { op.jt } else { op.jf } as usize,
            0x35 => pc += if acc >= op.k { op.jt } else { op.jf } as usize,
            0x05 => pc += op.k as usize,
            0x06 => return op.k,
            code => panic!("unsupported test BPF opcode {code}"),
        }
    }
    panic!("BPF failed to terminate")
}
fn result(programs: &[BpfProgram], nr: u32, args: [u64; 6]) -> u32 {
    let arch = if cfg!(target_arch = "x86_64") {
        0xc000003e
    } else if cfg!(target_arch = "aarch64") {
        0xc00000b7
    } else {
        0xc00000f3
    };
    programs
        .iter()
        .map(|p| verdict(p, nr, arch, args))
        .find(|v| *v != 0x7fff0000)
        .unwrap_or(0x7fff0000)
}
#[test]
fn compiled_filter_denies_all_socket_families_and_only_allows_unix_pairs() {
    let programs = SeccompPlan::for_host().unwrap().compile().unwrap();
    let (socket, pair) = if cfg!(target_arch = "x86_64") {
        (41, 53)
    } else {
        (198, 199)
    };
    for family in [0, 1, 2, 10, 16, 17, 38, 999, u32::MAX] {
        for kind in [0, 1, 2, 3, 5, 10, 0x80001, u32::MAX] {
            assert_eq!(
                result(&programs, socket, [family.into(), kind.into(), 0, 0, 0, 0]),
                0x50001
            );
            assert_eq!(
                result(&programs, pair, [family.into(), kind.into(), 0, 0, 0, 0]),
                if family == 1 { 0x7fff0000 } else { 0x50001 }
            );
        }
    }
}

#[test]
fn ipc_process_access_and_namespace_syscalls_fail_with_errno() {
    let programs = SeccompPlan::for_host().unwrap().compile().unwrap();
    let denied = if cfg!(target_arch = "x86_64") {
        vec![
            29, 30, 31, 64, 65, 66, 67, 68, 69, 70, 71, 101, 220, 240, 241, 242, 243, 244, 245,
            272, 308, 310, 311, 312, 425, 426, 427, 438,
        ]
    } else {
        vec![
            97, 117, 180, 181, 182, 183, 184, 185, 186, 187, 188, 189, 190, 191, 192, 193, 194,
            195, 196, 197, 268, 270, 271, 272, 425, 426, 427, 438,
        ]
    };
    for nr in denied {
        assert_eq!(result(&programs, nr, [0; 6]), 0x50001, "syscall {nr}");
    }
    assert_eq!(
        result(&programs, 435, [0; 6]),
        0x50026,
        "clone3 returns ENOSYS"
    );
    let clone = if cfg!(target_arch = "x86_64") {
        56
    } else {
        220
    };
    for flag in [
        0x80, 0x20000, 0x2000000, 0x4000000, 0x8000000, 0x10000000, 0x20000000, 0x40000000,
        0x7e020080,
    ] {
        assert_eq!(
            result(&programs, clone, [flag | 0x100, 0, 0, 0, 0, 0]),
            0x50001
        );
    }
    assert_eq!(
        result(&programs, clone, [0x100 | 0x200 | 17, 0, 0, 0, 0, 0]),
        0x7fff0000
    );
    assert_eq!(result(&programs, 0, [0; 6]), 0x7fff0000);
}

#[test]
fn alternate_abis_cannot_bypass_the_filter_and_denial_never_kills() {
    let programs = SeccompPlan::for_host().unwrap().compile().unwrap();
    for program in &programs {
        assert_eq!(verdict(program, 41, 0x40000003, [0; 6]), 0x50001);
    }
    if cfg!(target_arch = "x86_64") {
        for nr in [0x40000029, 0x4000021b, 0x4000021c, 0x40000000] {
            assert_eq!(result(&programs, nr, [0; 6]), 0x50001);
        }
    }
}

#[test]
fn compiled_filter_blocks_pathname_ipc_parent_limits_and_metadata_changes() {
    let programs = SeccompPlan::for_host().unwrap().compile().unwrap();
    let generic = !cfg!(target_arch = "x86_64");
    let denied: &[u32] = if generic {
        &[
            200, 203, 211, 269, 52, 53, 452, 54, 55, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 88,
            463, 464, 465, 466, 468, 469,
        ]
    } else {
        &[
            42, 49, 46, 307, 90, 91, 268, 452, 92, 93, 94, 260, 188, 189, 190, 191, 192, 193, 194,
            195, 196, 197, 198, 199, 280, 261, 463, 464, 465, 466, 468, 469,
        ]
    };
    for &nr in denied {
        assert_eq!(result(&programs, nr, [0; 6]), 0x50001, "syscall {nr}");
    }
    // file_getattr/file_setattr(AT_FDCWD, path, buffer, usize, at_flags) stay denied with real arguments.
    for nr in [468, 469] {
        assert_eq!(
            result(&programs, nr, [0xffff_ff9c, 0x7000, 0x7100, 24, 0, 0]),
            0x50001,
            "syscall {nr} with arguments"
        );
    }
    // Neighbours of 468/469 that are not listed keep the default Allow verdict.
    for nr in [467, 470] {
        assert_eq!(result(&programs, nr, [0; 6]), 0x7fff0000, "syscall {nr}");
    }
    let sendto = if generic { 206 } else { 44 };
    assert_eq!(result(&programs, sendto, [3, 0, 0, 0, 0, 0]), 0x7fff0000);
    for address in [1, 0x100000000, u64::MAX] {
        assert_eq!(result(&programs, sendto, [3, 0, 0, 0, address, 1]), 0x50001);
    }
    let prlimit = if generic { 261 } else { 302 };
    assert_eq!(result(&programs, prlimit, [0; 6]), 0x7fff0000);
    for pid in [1, 1234, u32::MAX as u64] {
        assert_eq!(result(&programs, prlimit, [pid, 7, 0, 0, 0, 0]), 0x50001);
    }
}

#[test]
fn installed_filter_protects_parent_limits_and_workspace_metadata() {
    use super::support::TempRoot;
    use std::os::unix::fs::MetadataExt;
    let root = TempRoot::new();
    std::fs::create_dir(root.0.join("workspace")).unwrap();
    let before = std::fs::metadata(&root.0).unwrap();
    let flags_before = inode_flags(&root.0);
    let path = root.0.clone();
    let parent = std::process::id().to_string();
    // Negative control, before any filter is installed: the file_getattr/file_setattr probes
    // are well-formed. Their verdict depends on the kernel and filesystem, so it is reported.
    let control = std::process::Command::new("/usr/bin/python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/isolation/metadata.py"
        ))
        .arg(&path)
        .arg(&parent)
        .arg("control")
        .output()
        .unwrap();
    assert!(control.status.success(), "{control:?}");
    eprintln!("{}", String::from_utf8_lossy(&control.stdout).trim_end());
    let status = std::thread::spawn(move || {
        for program in SeccompPlan::for_host().unwrap().compile().unwrap() {
            seccompiler::apply_filter(&program).unwrap();
        }
        std::process::Command::new("/usr/bin/python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/isolation/metadata.py"
            ))
            .arg(path)
            .arg(parent)
            .status()
            .unwrap()
    })
    .join()
    .unwrap();
    assert!(status.success());
    let after = std::fs::metadata(&root.0).unwrap();
    assert_eq!(
        (
            after.mode(),
            after.uid(),
            after.gid(),
            after.mtime(),
            after.mtime_nsec()
        ),
        (
            before.mode(),
            before.uid(),
            before.gid(),
            before.mtime(),
            before.mtime_nsec()
        )
    );
    match (flags_before, inode_flags(&root.0)) {
        (Ok(before), Ok(after)) => assert_eq!(before, after, "inode attribute flags changed"),
        (Err(reason), _) | (_, Err(reason)) => {
            eprintln!("SKIPPED inode attribute flag comparison: {reason}")
        }
    }
}

/// Inode attribute flags through FS_IOC_GETFLAGS, or the reason the filesystem cannot report them.
fn inode_flags(path: &std::path::Path) -> Result<u32, String> {
    let directory = std::fs::File::open(path).map_err(|e| e.to_string())?;
    rustix::fs::ioctl_getflags(&directory)
        .map(|flags| flags.bits())
        .map_err(|e| format!("FS_IOC_GETFLAGS: {e}"))
}

#[test]
fn installed_filter_denies_pathname_sockets_and_preserves_unnamed_pairs() {
    use super::support::TempRoot;
    use std::os::unix::net::UnixDatagram;
    let root = TempRoot::new();
    let destination = root.0.join("socket");
    let _listener = UnixDatagram::bind(&destination).unwrap();
    let unconnected = UnixDatagram::unbound().unwrap();
    let (left, right) = UnixDatagram::pair().unwrap();
    std::thread::spawn(move || {
        for program in SeccompPlan::for_host().unwrap().compile().unwrap() {
            seccompiler::apply_filter(&program).unwrap();
        }
        assert_eq!(
            unconnected
                .connect(&destination)
                .unwrap_err()
                .raw_os_error(),
            Some(1)
        );
        assert_eq!(
            unconnected
                .send_to(b"denied", &destination)
                .unwrap_err()
                .raw_os_error(),
            Some(1)
        );
        assert_eq!(left.send(b"pair").unwrap(), 4);
        let mut bytes = [0; 4];
        assert_eq!(right.recv(&mut bytes).unwrap(), 4);
        assert_eq!(&bytes, b"pair");
        assert_eq!(rustix::io::write(&left, b"write").unwrap(), 5);
        let mut bytes = [0; 5];
        assert_eq!(right.recv(&mut bytes).unwrap(), 5);
        assert_eq!(&bytes, b"write");
    })
    .join()
    .unwrap();
}
