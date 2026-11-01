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

const DENIED: u32 = 0x50001;
const ALLOWED: u32 = 0x7fff0000;

// Independent of the production table: numbers from the Linux UAPI tables
// (asm/unistd_64.h and asm-generic/unistd.h), one literal per architecture.
fn arch(x86_64: u32, generic: u32) -> u32 {
    if cfg!(target_arch = "x86_64") {
        x86_64
    } else {
        generic
    }
}

#[test]
fn connect_bind_and_destination_bearing_sends_are_denied_but_null_destination_sends_pass() {
    let programs = SeccompPlan::for_host().unwrap().compile().unwrap();
    let (connect, bind, sendto) = (arch(42, 203), arch(49, 200), arch(44, 206));
    let (sendmsg, sendmmsg) = (arch(46, 211), arch(307, 269));
    for nr in [connect, bind, sendmsg, sendmmsg] {
        assert_eq!(result(&programs, nr, [0; 6]), DENIED, "syscall {nr}");
        assert_eq!(
            result(&programs, nr, [3, 1, 2, 3, 4, 5]),
            DENIED,
            "syscall {nr}"
        );
    }
    // sendto(fd, buf, len, flags, dest_addr, addrlen): argument 4 is a pointer.
    assert_eq!(result(&programs, sendto, [3, 0x1000, 8, 0, 0, 0]), ALLOWED);
    assert_eq!(
        result(&programs, sendto, [3, 0x1000, 8, 0, 0, 110]),
        ALLOWED
    );
    for dest in [1, 0x7fff_0000, 1 << 32, 0x7fff_1234_5678_0000, u64::MAX] {
        assert_eq!(
            result(&programs, sendto, [3, 0x1000, 8, 0, dest, 110]),
            DENIED,
            "dest_addr {dest:#x}"
        );
    }
}

#[test]
fn prlimit64_is_denied_for_any_other_process_and_allowed_for_self() {
    let programs = SeccompPlan::for_host().unwrap().compile().unwrap();
    let prlimit64 = arch(302, 261);
    // prlimit64(pid, resource, new_limit, old_limit)
    assert_eq!(result(&programs, prlimit64, [0, 7, 0, 0, 0, 0]), ALLOWED);
    assert_eq!(
        result(&programs, prlimit64, [0, 7, 0x1000, 0x2000, 0, 0]),
        ALLOWED
    );
    for pid in [1, 2, 4242, 0x7fff_ffff, u32::MAX] {
        assert_eq!(
            result(&programs, prlimit64, [pid.into(), 7, 0x1000, 0, 0, 0]),
            DENIED,
            "pid {pid}"
        );
    }
}

#[test]
fn filesystem_metadata_mutators_are_denied() {
    let programs = SeccompPlan::for_host().unwrap().compile().unwrap();
    let mut denied = vec![
        // fchmod fchmodat fchmodat2 fchown fchownat setxattr lsetxattr fsetxattr
        // removexattr lremovexattr fremovexattr utimensat setxattrat removexattrat
        arch(91, 52),
        arch(268, 53),
        452,
        arch(93, 55),
        arch(260, 54),
        arch(188, 5),
        arch(189, 6),
        arch(190, 7),
        arch(197, 14),
        arch(198, 15),
        arch(199, 16),
        arch(280, 88),
        463,
        466,
    ];
    if cfg!(target_arch = "x86_64") {
        // chmod chown lchown utime utimes futimesat exist only in the x86_64 table.
        denied.extend([90, 92, 94, 132, 235, 261]);
    }
    for nr in denied {
        assert_eq!(result(&programs, nr, [0; 6]), DENIED, "syscall {nr}");
        assert_eq!(
            result(&programs, nr, [3, 4, 5, 6, 7, 8]),
            DENIED,
            "syscall {nr}"
        );
    }
    // Reading metadata stays available: getxattr and fstat-family syscalls.
    for nr in [arch(191, 8), arch(5, 80)] {
        assert_eq!(result(&programs, nr, [0; 6]), ALLOWED, "syscall {nr}");
    }
}
