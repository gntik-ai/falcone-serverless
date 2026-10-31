use landlock::{ABI, Access, AccessFs, AccessNet, BitFlags, CompatLevel, Scope};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Configuration,
    Landlock,
    Seccomp,
    CredentialMount,
    Probe,
    Privileges,
    Limits,
    Descriptors,
    Workspace,
    Spawn,
    Control,
}
pub type Result<T> = std::result::Result<T, Failure>;

/// These are mount-presence probes, never files to open or credential values to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialProbe {
    KubernetesIdentity,
    RunSecrets,
    VarRunSecrets,
    ServiceIdentity,
    ServiceConfiguration,
}
impl CredentialProbe {
    pub const ALL: [Self; 5] = [
        Self::KubernetesIdentity,
        Self::RunSecrets,
        Self::VarRunSecrets,
        Self::ServiceIdentity,
        Self::ServiceConfiguration,
    ];
    pub fn path(self) -> &'static Path {
        Path::new(match self {
            Self::KubernetesIdentity => "/var/run/secrets/kubernetes.io/serviceaccount",
            Self::RunSecrets => "/run/secrets",
            Self::VarRunSecrets => "/var/run/secrets",
            Self::ServiceIdentity => "/etc/falcone/identity",
            Self::ServiceConfiguration => "/etc/falcone/compiler",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Rust,
    Go,
    JavaScript,
    Python,
}
impl Profile {
    pub const ALL: [Self; 4] = [Self::Rust, Self::Go, Self::JavaScript, Self::Python];
    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Go => "go",
            Self::JavaScript => "javascript",
            Self::Python => "python",
        }
    }
    pub fn parse(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|p| p.name() == value)
            .ok_or(Failure::Configuration)
    }
    /// Reviewed additions belong to the language profile matrix (#97).
    pub fn extra_paths(self) -> &'static [ExtraPath] {
        &[]
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtraPath {
    pub path: &'static str,
    pub access: BitFlags<AccessFs>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub address_space: u64,
    pub cpu_seconds: u64,
    pub open_files: u64,
    pub file_size: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            address_space: 2 * 1024 * 1024 * 1024,
            cpu_seconds: 60,
            open_files: 128,
            file_size: 64 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Debug)]
pub struct RunnerConfig {
    pub toolchain_dir: PathBuf,
    pub catalogue_dir: PathBuf,
    pub workspace_root: PathBuf,
    pub control_socket: String,
    pub landlock_min_abi: u32,
    pub limits: Limits,
}
impl RunnerConfig {
    pub fn new(
        toolchain: impl Into<PathBuf>,
        catalogue: impl Into<PathBuf>,
        workspace: impl Into<PathBuf>,
        socket: impl Into<String>,
    ) -> Self {
        Self {
            toolchain_dir: toolchain.into(),
            catalogue_dir: catalogue.into(),
            workspace_root: workspace.into(),
            control_socket: socket.into(),
            landlock_min_abi: 6,
            limits: Limits::default(),
        }
    }
    /// Positional, credential-free deployment inputs. No environment/config bypass keys.
    pub fn arguments(&self) -> Vec<OsString> {
        vec![
            self.toolchain_dir.clone().into_os_string(),
            self.catalogue_dir.clone().into_os_string(),
            self.workspace_root.clone().into_os_string(),
            self.control_socket.clone().into(),
            self.landlock_min_abi.to_string().into(),
            self.limits.address_space.to_string().into(),
            self.limits.cpu_seconds.to_string().into(),
            self.limits.open_files.to_string().into(),
            self.limits.file_size.to_string().into(),
        ]
    }
    pub fn from_arguments(args: &[OsString]) -> Result<Self> {
        if args.len() != 9 {
            return Err(Failure::Configuration);
        }
        let number = |index: usize| -> Result<u64> {
            args[index]
                .to_str()
                .ok_or(Failure::Configuration)?
                .parse()
                .map_err(|_| Failure::Configuration)
        };
        let config = Self {
            toolchain_dir: args[0].clone().into(),
            catalogue_dir: args[1].clone().into(),
            workspace_root: args[2].clone().into(),
            control_socket: args[3].to_str().ok_or(Failure::Configuration)?.into(),
            landlock_min_abi: number(4)?.try_into().map_err(|_| Failure::Configuration)?,
            limits: Limits {
                address_space: number(5)?,
                cpu_seconds: number(6)?,
                open_files: number(7)?,
                file_size: number(8)?,
            },
        };
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        if self.landlock_min_abi < 6
            || self.control_socket.is_empty()
            || self.control_socket.len() > 107
            || self.control_socket.contains(['/', '\0'])
            || self.limits.address_space == 0
            || self.limits.cpu_seconds == 0
            || self.limits.open_files < 3
            || self.limits.file_size == 0
        {
            return Err(Failure::Configuration);
        }
        let roots = [
            &self.toolchain_dir,
            &self.catalogue_dir,
            &self.workspace_root,
        ];
        for (i, root) in roots.iter().enumerate() {
            if !normalized(root)
                || [
                    "/", "/usr", "/lib", "/lib64", "/tmp", "/etc", "/run", "/var",
                ]
                .iter()
                .any(|p| root.as_path() == Path::new(p))
                || ["/usr", "/lib", "/lib64", "/proc", "/sys", "/dev"]
                    .iter()
                    .any(|p| root.starts_with(p))
                || CredentialProbe::ALL
                    .iter()
                    .any(|p| p.path().starts_with(root) || root.starts_with(p.path()))
                || roots.iter().enumerate().any(|(j, other)| {
                    i != j && (root.starts_with(other) || other.starts_with(root))
                })
            {
                return Err(Failure::Configuration);
            }
        }
        Ok(())
    }
}
fn normalized(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathRule {
    pub path: PathBuf,
    pub access: BitFlags<AccessFs>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandlockPlan {
    pub abi: ABI,
    pub minimum_abi: u32,
    pub compatibility: CompatLevel,
    pub handled_fs: BitFlags<AccessFs>,
    pub handled_net: BitFlags<AccessNet>,
    pub scopes: BitFlags<Scope>,
    pub paths: Vec<PathRule>,
}
impl LandlockPlan {
    pub fn for_build(config: &RunnerConfig, workspace: &Path, profile: Profile) -> Result<Self> {
        config.validate()?;
        if !normalized(workspace) || workspace.parent() != Some(config.workspace_root.as_path()) {
            return Err(Failure::Workspace);
        }
        let read = AccessFs::ReadFile | AccessFs::ReadDir;
        let mut paths = vec![
            PathRule {
                path: config.toolchain_dir.clone(),
                access: read | AccessFs::Execute,
            },
            PathRule {
                path: config.catalogue_dir.clone(),
                access: read,
            },
            PathRule {
                path: workspace.into(),
                access: AccessFs::from_all(ABI::V6) & !AccessFs::Execute,
            },
            PathRule {
                path: "/dev/null".into(),
                access: AccessFs::ReadFile | AccessFs::WriteFile,
            },
            PathRule {
                path: "/proc/self".into(),
                access: read,
            },
        ];
        paths.extend(profile.extra_paths().iter().map(|extra| PathRule {
            path: extra.path.into(),
            access: extra.access,
        }));
        Ok(Self {
            abi: ABI::V6,
            minimum_abi: config.landlock_min_abi,
            compatibility: CompatLevel::HardRequirement,
            handled_fs: AccessFs::from_all(ABI::V6),
            handled_net: AccessNet::BindTcp | AccessNet::ConnectTcp,
            scopes: Scope::Signal | Scope::AbstractUnixSocket,
            paths,
        })
    }
}
