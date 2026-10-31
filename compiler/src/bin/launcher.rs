//! Build-runner entrypoint. No mocks or runtime check-disabling options are linked here.
//! Deployment arguments: toolchain_dir catalogue_dir workspace_root control_socket
//! landlock_min_abi address_space_bytes cpu_seconds open_files file_size_bytes.
use falcone_serverless_compiler::isolate::*;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

fn run() -> Result<()> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let executable = std::env::current_exe().map_err(|_| Failure::Configuration)?;
    let mut kernel = ProductionKernel::new(executable);
    if args
        .first()
        .is_some_and(|arg| arg == "--child" || arg == "--probe")
    {
        if args.len() != 12 {
            return Err(Failure::Configuration);
        }
        let config = RunnerConfig::from_arguments(&args[1..10])?;
        let workspace = PathBuf::from(&args[10]);
        let profile = Profile::parse(args[11].to_str().ok_or(Failure::Configuration)?)?;
        let spec = SpawnSpec::for_build(&config, &workspace, profile)?;
        check_runner_environment(&mut kernel, &config)?;
        if args[0] == "--probe" {
            apply_confinement(&mut kernel, &spec)
        } else {
            confine_and_exec(&mut kernel, &spec)
        }
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
