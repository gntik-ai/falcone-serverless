//! Exercise OS accept failures without changing limits in the test harness process.
use falcone_serverless_worker::server::{Listener, Phase};
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

#[path = "../src/qualification/execution_api.rs"]
pub mod cases;

#[test]
fn listeners_recover_after_file_descriptor_exhaustion() {
    if std::env::var_os("RUN01_ACCEPT_CHILD").is_some() {
        let fixture = cases::Fixture::new(
            include_bytes!("../src/qualification/execution_api_fixtures/success.wasm"),
            1,
            30000,
        );
        let original = rustix::process::getrlimit(rustix::process::Resource::Nofile);
        rustix::process::setrlimit(
            rustix::process::Resource::Nofile,
            rustix::process::Rlimit {
                current: Some(128),
                maximum: original.maximum,
            },
        )
        .unwrap();
        let mut files = Vec::new();
        loop {
            match std::fs::File::open("/dev/null") {
                Ok(file) => files.push(file),
                Err(error) => {
                    assert_eq!(
                        error.raw_os_error(),
                        Some(rustix::io::Errno::MFILE.raw_os_error())
                    ); // EMFILE
                    break;
                }
            }
        }
        println!(
            "accept-ready {} {}",
            fixture.server.execute_addr(),
            fixture.server.control_addr()
        );
        std::io::stdout().flush().unwrap();
        let mut failed = [false; 2];
        while failed != [true; 2] {
            let event = fixture
                .server
                .next_event(Duration::from_secs(7))
                .expect("accept failure event");
            if event.phase == Phase::AcceptError {
                failed[if event.listener == Listener::Execute {
                    0
                } else {
                    1
                }] = true;
            }
        }
        drop(files);
        rustix::process::setrlimit(rustix::process::Resource::Nofile, original).unwrap();
        // Both pending connections must be accepted after the external fault clears.
        let mut closed = [false; 2];
        while closed != [true; 2] {
            let event = fixture
                .server
                .next_event(Duration::from_secs(7))
                .expect("listener did not recover");
            if event.phase == Phase::Closed {
                closed[if event.listener == Listener::Execute {
                    0
                } else {
                    1
                }] = true;
            }
        }
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "listeners_recover_after_file_descriptor_exhaustion",
            "--nocapture",
        ])
        .env("RUN01_ACCEPT_CHILD", "1")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(
            output.read_line(&mut line).unwrap(),
            0,
            "child exited before readiness"
        );
        if line.starts_with("accept-ready ") {
            break;
        }
    }
    let addresses: Vec<_> = line.split_whitespace().skip(1).collect();
    let mut streams = Vec::new();
    for address in addresses {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(7)))
            .unwrap();
        // A route refusal needs neither artifact access nor guest execution.
        write!(
            stream,
            "POST /unknown HTTP/1.1\r\nContent-Length: 0\r\n\r\n"
        )
        .unwrap();
        streams.push(stream);
    }
    for stream in &mut streams {
        assert_eq!(cases::response(stream).0, 404);
    }
    // Drain the pipe before waiting, including the child test harness output.
    std::io::copy(&mut output, &mut std::io::sink()).unwrap();
    assert!(child.wait().unwrap().success());
}
