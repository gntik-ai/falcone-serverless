//! Test-only local reference, not copied into the worker image. Never runs tenant code.
use serde_json::{Value, json};
use std::{
    env, fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::process::CommandExt,
    process::Command,
    thread,
    time::Duration,
};

const LIMIT: usize = 6_000_000;
const LOG_LIMIT: usize = 256_000;

/// RFC 3339 UTC with a mandatory `Z` and 1-9 optional fractional digits, as published in
/// `context.schema.json`. Written independently of the tester so the two can disagree.
pub fn valid_deadline(text: &str) -> bool {
    let Some(body) = text.strip_suffix('Z') else {
        return false;
    };
    let (whole, fraction) = match body.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (body, None),
    };
    if fraction
        .is_some_and(|f| !(1..=9).contains(&f.len()) || !f.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    let Some((date, time)) = whole.split_once('T') else {
        return false;
    };
    let fields = |text: &str, separator: char, widths: &[usize]| -> Option<Vec<u32>> {
        let parts: Vec<_> = text.split(separator).collect();
        (parts.len() == widths.len()
            && parts.iter().zip(widths).all(|(part, width)| {
                part.len() == *width && part.bytes().all(|b| b.is_ascii_digit())
            }))
        .then(|| parts.iter().map(|part| part.parse().unwrap()).collect())
    };
    let (Some(date), Some(time)) = (fields(date, '-', &[4, 2, 2]), fields(time, ':', &[2, 2, 2]))
    else {
        return false;
    };
    let (year, month, day) = (date[0], date[1], date[2]);
    let leap_year = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_length = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => 28 + u32::from(leap_year),
        _ => return false,
    };
    let (hour, minute, second) = (time[0], time[1], time[2]);
    (1..=month_length).contains(&day)
        && hour <= 23
        && minute <= 59
        && (second <= 59 || (second == 60 && hour == 23 && minute == 59))
}

fn valid_request(value: &Value, literal_deadline_only: bool) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some(context) = value.get("context").and_then(Value::as_object) else {
        return false;
    };
    object.len() == 2
        && object.contains_key("params")
        && context.len() == 3
        && context
            .get("handle")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty() && s.chars().count() <= 4096)
        && context
            .get("invocationId")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty() && s.chars().count() <= 256)
        && context
            .get("deadline")
            .and_then(Value::as_str)
            .is_some_and(|s| {
                if literal_deadline_only {
                    s == "2026-10-31T12:00:00Z"
                } else {
                    valid_deadline(s)
                }
            })
}

fn serve(mut stream: TcpStream, mode: &str) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut bytes = Vec::new();
    let end = loop {
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break bytes.len();
        }
        if bytes.len() > 16_384 {
            return Ok(());
        }
    };
    let header = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
    let length = header
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(0);
    if length > 7_000_000 {
        return Ok(());
    }
    bytes.resize(end + length, 0);
    stream.read_exact(&mut bytes[end..])?;
    let first = header.lines().next().unwrap_or("");
    let mut status = 200;
    let mut body = json!({"status":"success", "result":{"message":"hello"}, "logs":""});
    if first == "get /healthz http/1.1" {
        if mode == "health-hang" {
            thread::sleep(Duration::from_secs(60));
        }
        if mode == "OCI-V1-HEALTHZ" {
            status = 503;
        }
    } else if first != "post /invoke http/1.1" {
        status = if mode == "OCI-V1-INVOKE" { 200 } else { 404 };
    } else {
        let request: Option<Value> = serde_json::from_slice(&bytes[end..]).ok();
        if mode != "OCI-V1-REQUEST"
            && (!header.contains("content-type: application/json\r\n")
                || !request
                    .as_ref()
                    .is_some_and(|v| valid_request(v, mode == "fixed-deadline")))
        {
            status = 400;
        } else if mode != "OCI-V1-PARAMS-LIMIT"
            && request
                .as_ref()
                .is_some_and(|v| serde_json::to_vec(&v["params"]).unwrap().len() > LIMIT)
        {
            status = 413;
        } else {
            match mode {
                "OCI-V1-ENVELOPE" => {
                    body.as_object_mut().unwrap().remove("result");
                }
                "OCI-V1-STATUS" => body["status"] = json!("pending"),
                "OCI-V1-RESULT-LIMIT" => body["result"] = json!("x".repeat(LIMIT - 1)),
                "result-boundary" => body["result"] = json!("x".repeat(LIMIT - 2)),
                "OCI-V1-LOGS-LIMIT" => body["logs"] = json!("x".repeat(LOG_LIMIT + 1)),
                "OCI-V1-LOGS-TRUNCATION" => body["logs"] = json!("x".repeat(LOG_LIMIT)),
                "logs-boundary" => {
                    body["logs"] = json!(format!("{}[TRUNCATED]", "x".repeat(LOG_LIMIT - 11)))
                }
                "logs-unicode" => body["logs"] = json!("é".repeat(LOG_LIMIT / 2 + 1)),
                "replay-success" => {
                    body = serde_json::from_str(include_str!(
                        "../oci_contract_assets/fixtures/legacy/success.json"
                    ))
                    .unwrap()
                }
                "replay-error" => {
                    body = serde_json::from_str(include_str!(
                        "../oci_contract_assets/fixtures/legacy/error.json"
                    ))
                    .unwrap()
                }
                "replay-null" => {
                    body = serde_json::from_str(include_str!(
                        "../oci_contract_assets/fixtures/legacy/null.json"
                    ))
                    .unwrap()
                }
                _ => {}
            }
        }
    }
    let body = serde_json::to_vec(&body).unwrap();
    write!(
        stream,
        "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)
}

/// Modes whose process must start with SIGTERM ignored (inherited through `exec`).
fn ignores_sigterm(mode: &str) -> bool {
    matches!(mode, "ignore-sigterm" | "leave-group" | "idle-leave-group")
}

/// Joins the tester's process group (same session), so the original group of this
/// process disappears and a group kill of it returns ESRCH.
fn leave_process_group() {
    use rustix::process::{getpgid, getppid, setpgid};
    let parent = getppid().expect("tester parent");
    setpgid(None, Some(getpgid(Some(parent)).unwrap())).unwrap();
}

pub fn main() {
    let mode = env::args().nth(1).unwrap_or_else(|| "compliant".into());
    if ignores_sigterm(&mode) && env::args().nth(2).as_deref() != Some("inherited") {
        let error = Command::new("/bin/sh")
            .args(["-c", "trap '' TERM; exec \"$@\"", "reference"])
            .arg(env::current_exe().unwrap())
            .args([mode.as_str(), "inherited"])
            .exec();
        panic!("exec: {error}");
    }
    if mode == "hold-listener" {
        // Descendant of an extra-listener-child candidate: report readiness on stdout.
        let _held = TcpListener::bind(("0.0.0.0", 0)).unwrap();
        println!("ready");
        loop {
            thread::sleep(Duration::from_secs(60));
        }
    }
    if matches!(mode.as_str(), "leave-group" | "idle-leave-group") {
        leave_process_group();
    }
    if mode == "idle-leave-group" {
        // No listener: lets lifecycle tests run where local sockets are unavailable.
        fs::write(
            std::path::Path::new(&env::var_os("TMPDIR").unwrap()).join("ready"),
            b"ready",
        )
        .unwrap();
        loop {
            thread::sleep(Duration::from_secs(60));
        }
    }
    let port: u16 = env::var("PORT").expect("PORT mandatory").parse().unwrap();
    let listener = TcpListener::bind((
        if mode == "OCI-V1-BINDING" {
            "0.0.0.0"
        } else {
            "127.0.0.1"
        },
        port,
    ))
    .unwrap();
    // Held for the process lifetime: extra listeners the candidate must not have.
    let _extra = match mode.as_str() {
        "extra-listener" => Some(TcpListener::bind(("0.0.0.0", 0)).unwrap()),
        "extra-listener-loopback" => Some(TcpListener::bind(("127.0.0.1", 0)).unwrap()),
        _ => None,
    };
    let _descendant = (mode == "extra-listener-child").then(|| {
        let mut child = Command::new(env::current_exe().unwrap())
            .arg("hold-listener")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = [0; 6];
        child
            .stdout
            .as_mut()
            .unwrap()
            .read_exact(&mut ready)
            .unwrap();
        child
    });
    match mode.as_str() {
        "OCI-V1-FILESYSTEM" | "write-cwd" => fs::write("created", b"violation").unwrap(),
        "write-home" => fs::write(
            std::path::Path::new(&env::var_os("HOME").unwrap()).join("created"),
            b"violation",
        )
        .unwrap(),
        "modify-sentinel" => fs::write("unchanged", b"changed").unwrap(),
        "remove-sentinel" => fs::remove_file("unchanged").unwrap(),
        "write-tmp" => fs::write(
            std::path::Path::new(&env::var_os("TMPDIR").unwrap()).join("allowed"),
            b"scratch",
        )
        .unwrap(),
        _ => {}
    }
    for stream in listener.incoming() {
        let mode = mode.clone();
        thread::spawn(move || {
            let _ = serve(stream.unwrap(), &mode);
        });
    }
}
