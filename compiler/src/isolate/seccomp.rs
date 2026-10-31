use super::{Failure, Result};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Condition {
    Always,
    ArgumentNotEqual { index: u8, value: u64 },
    ArgumentHasBits { index: u8, mask: u64 },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DenyRule {
    pub syscall: &'static str,
    pub number: i64,
    pub condition: Condition,
    pub errno: u32,
}
#[derive(Clone, Debug)]
pub struct SeccompPlan {
    pub arch: TargetArch,
    pub rules: Vec<DenyRule>,
}
impl SeccompPlan {
    pub fn for_host() -> Result<Self> {
        let arch = std::env::consts::ARCH
            .try_into()
            .map_err(|_| Failure::Seccomp)?;
        let (socket, socketpair) = if cfg!(target_arch = "x86_64") {
            (41, 53)
        } else {
            (198, 199)
        };
        let mut plan = Self {
            arch,
            rules: vec![
                DenyRule {
                    syscall: "socket",
                    number: socket,
                    condition: Condition::Always,
                    errno: 1,
                },
                DenyRule {
                    syscall: "socketpair",
                    number: socketpair,
                    condition: Condition::ArgumentNotEqual { index: 0, value: 1 },
                    errno: 1,
                },
            ],
        };
        // Linux UAPI syscall numbers. aarch64 and riscv64 use asm-generic/unistd.h;
        // x86_64 uses asm/unistd_64.h. No runtime architecture selection is accepted.
        let table = [
            ("shmget", 29, 194),
            ("shmat", 30, 196),
            ("shmctl", 31, 195),
            ("shmdt", 67, 197),
            ("semget", 64, 190),
            ("semop", 65, 193),
            ("semctl", 66, 191),
            ("semtimedop", 220, 192),
            ("msgget", 68, 186),
            ("msgsnd", 69, 189),
            ("msgrcv", 70, 188),
            ("msgctl", 71, 187),
            ("mq_open", 240, 180),
            ("mq_unlink", 241, 181),
            ("mq_timedsend", 242, 182),
            ("mq_timedreceive", 243, 183),
            ("mq_notify", 244, 184),
            ("mq_getsetattr", 245, 185),
            ("ptrace", 101, 117),
            ("process_vm_readv", 310, 270),
            ("process_vm_writev", 311, 271),
            ("kcmp", 312, 272),
            ("pidfd_getfd", 438, 438),
            ("unshare", 272, 97),
            ("setns", 308, 268),
            ("io_uring_setup", 425, 425),
            ("io_uring_enter", 426, 426),
            ("io_uring_register", 427, 427),
        ];
        plan.rules
            .extend(table.into_iter().map(|(syscall, x86, generic)| DenyRule {
                syscall,
                number: if cfg!(target_arch = "x86_64") {
                    x86
                } else {
                    generic
                },
                condition: Condition::Always,
                errno: 1,
            }));
        plan.rules.push(DenyRule {
            syscall: "clone3",
            number: 435,
            condition: Condition::Always,
            errno: 38,
        });
        plan.rules.push(DenyRule {
            syscall: "clone",
            number: if cfg!(target_arch = "x86_64") {
                56
            } else {
                220
            },
            condition: Condition::ArgumentHasBits {
                index: 0,
                mask: 0x7e02_0080,
            },
            errno: 1,
        });
        Ok(plan)
    }
    /// Default Allow deliberately stacks on the container's RuntimeDefault filter.
    pub fn compile(&self) -> Result<Vec<BpfProgram>> {
        let mut filters = Vec::new();
        for errno in [1, 38] {
            let mut rules = BTreeMap::new();
            for rule in self.rules.iter().filter(|r| r.errno == errno) {
                let conditions = match rule.condition {
                    Condition::Always => Vec::new(),
                    Condition::ArgumentNotEqual { index, value } => vec![
                        SeccompCondition::new(
                            index,
                            SeccompCmpArgLen::Dword,
                            SeccompCmpOp::Ne,
                            value,
                        )
                        .map_err(|_| Failure::Seccomp)?,
                    ],
                    Condition::ArgumentHasBits { index, mask } => {
                        // Each set bit is an OR rule: deny ANY namespace flag, including combinations.
                        let alternatives = (0..32)
                            .filter(|bit| mask & (1 << bit) != 0)
                            .map(|bit| {
                                SeccompRule::new(vec![SeccompCondition::new(
                                    index,
                                    SeccompCmpArgLen::Dword,
                                    SeccompCmpOp::MaskedEq(1 << bit),
                                    1 << bit,
                                )?])
                            })
                            .collect::<std::result::Result<Vec<_>, _>>()
                            .map_err(|_| Failure::Seccomp)?;
                        rules.insert(rule.number, alternatives);
                        continue;
                    }
                };
                rules.insert(
                    rule.number,
                    if conditions.is_empty() {
                        vec![]
                    } else {
                        vec![SeccompRule::new(conditions).map_err(|_| Failure::Seccomp)?]
                    },
                );
            }
            if !rules.is_empty() {
                let mut program: BpfProgram = SeccompFilter::new(
                    rules,
                    SeccompAction::Allow,
                    SeccompAction::Errno(errno),
                    self.arch,
                )
                .map_err(|_| Failure::Seccomp)?
                .try_into()
                .map_err(|_| Failure::Seccomp)?;
                // Architecture mismatch must also return errno rather than kill.
                for op in &mut program {
                    if op.code == 0x06 && (op.k == 0 || op.k == 0x8000_0000) {
                        op.k = 0x0005_0001;
                    }
                }
                if cfg!(target_arch = "x86_64") {
                    // x32 shares AUDIT_ARCH_X86_64 but has a different syscall table.
                    // Reject the entire alternate ABI, including x32-only syscall numbers.
                    let mut prefix = vec![
                        seccompiler::sock_filter {
                            code: 0x20,
                            jt: 0,
                            jf: 0,
                            k: 0,
                        },
                        seccompiler::sock_filter {
                            code: 0x35,
                            jt: 0,
                            jf: 1,
                            k: 0x4000_0000,
                        },
                        seccompiler::sock_filter {
                            code: 0x06,
                            jt: 0,
                            jf: 0,
                            k: 0x0005_0001,
                        },
                    ];
                    prefix.extend(program);
                    program = prefix;
                }
                filters.push(program);
            }
        }
        Ok(filters)
    }
}
