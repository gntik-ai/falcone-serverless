//! Build-runner entrypoint. No mocks or runtime check-disabling options are linked here.
//! Deployment arguments: toolchain_dir catalogue_dir workspace_root control_socket
//! landlock_min_abi address_space_bytes cpu_seconds open_files file_size_bytes
//! forbidden_path... (one or more deployment credential mount paths, probed for presence).
use falcone_serverless_compiler::isolate::*;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

/// `--child|--probe` then the config arguments, the workspace and the profile.
fn run_trampoline(kernel: &mut ProductionKernel, args: &[OsString], child: bool) -> Result<()> {
    if args.len() < 13 {
        return Err(Failure::Configuration);
    }
    let config = RunnerConfig::from_arguments(&args[1..args.len() - 2])?;
    let workspace = PathBuf::from(&args[args.len() - 2]);
    let profile = Profile::parse(
        args[args.len() - 1]
            .to_str()
            .ok_or(Failure::Configuration)?,
    )?;
    let spec = SpawnSpec::for_build(&config, &workspace, profile)?;
    check_runner_environment(kernel, &config)?;
    if child {
        confine_and_exec(kernel, &spec)
    } else {
        apply_confinement(kernel, &spec)
    }
}
fn run() -> Result<()> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let executable = std::env::current_exe().map_err(|_| Failure::Configuration)?;
    let mut kernel = ProductionKernel::new(executable);
    if args
        .first()
        .is_some_and(|arg| arg == "--child" || arg == "--probe")
    {
        let child = args[0] == "--child";
        if child {
            // Before anything can fail: from here every failure is reported to the parent.
            kernel.adopt_setup_status()?;
        }
        let result = run_trampoline(&mut kernel, &args, child);
        if let (Err(failure), true) = (result, child) {
            kernel.report_setup_failure(failure);
        }
        result
    } else {
        let config = RunnerConfig::from_arguments(&args)?;
        let launcher = Launcher::new(config, kernel);
        let mut server = ControlServer::new(launcher, AbstractControl::default());
        server.start()?;
        while server.tick()? {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Ok(())
    }
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("build runner not ready");
            ExitCode::FAILURE
        }
    }
}
