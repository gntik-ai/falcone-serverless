use super::{Connection, Listener, Phase, State, run};
use crate::api::Refusal;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) fn busy(mut stream: TcpStream) {
    // Overflow owns no worker thread or permit and never reads request bytes.
    let body = serde_json::to_vec(&Refusal::busy()).unwrap();
    let response = format!(
        "HTTP/1.1 503 Busy\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        String::from_utf8(body).unwrap()
    );
    if stream.set_nonblocking(true).is_ok() {
        let _ = stream.write(response.as_bytes());
    }
    let _ = stream.shutdown(Shutdown::Both);
}

pub(super) fn serve(mut stream: TcpStream, state: Arc<State>, permit: Connection) {
    // Keep a maximum log response larger than the kernel's queued output, so
    // backpressure reaches the absolute write deadline on every qualified host.
    if rustix::net::sockopt::set_socket_send_buffer_size(&stream, 64 * 1024).is_err() {
        return;
    }
    let listener = permit.listener;
    if let Ok(header) = headers(&mut stream) {
        state.event(listener, Phase::Headers);
        let execute = listener == Listener::Execute && header.path == "/v1/execute";
        let control = listener == Listener::Control
            && matches!(header.path.as_str(), "/v1/cancel" | "/v1/update");
        let _execution = if execute && !header.non_wasm {
            state.execution()
        } else {
            None
        };
        let (status, value) = if !execute && !control {
            (404, serde_json::to_value(Refusal::not_found()).unwrap())
        } else if execute && header.non_wasm {
            (
                409,
                serde_json::to_value(Refusal::runtime_mode_not_supported()).unwrap(),
            )
        } else if execute && _execution.is_none() {
            (503, serde_json::to_value(Refusal::busy()).unwrap())
        } else if header.length > 8 * 1024 * 1024 || (control && header.length > 16384) {
            (
                413,
                serde_json::to_value(Refusal::payload_too_large()).unwrap(),
            )
        } else {
            state.event(listener, Phase::Body);
            let mut body = vec![0; header.length];
            if read_body(&mut stream, &mut body).is_err() {
                let _ = stream.shutdown(Shutdown::Both);
                return;
            }
            if execute {
                run::execute(&state, &body)
            } else {
                run::control(&state, &header.path, &body)
            }
        };
        state.event(listener, Phase::ResponseStarted);
        let _ = write(&mut stream, status, &value);
    }
    let _ = stream.shutdown(Shutdown::Both);
}
struct Header {
    path: String,
    length: usize,
    non_wasm: bool,
}
fn headers(stream: &mut TcpStream) -> std::io::Result<Header> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        header.push(byte[0]);
        if header.len() > 16384 {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
    }
    let header = std::str::from_utf8(&header).map_err(|_| std::io::ErrorKind::InvalidData)?;
    let mut lines = header.split("\r\n");
    let parts: Vec<_> = lines.next().unwrap_or("").split(' ').collect();
    if parts.len() != 3 || parts[0] != "POST" || parts[2] != "HTTP/1.1" {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let path = parts[1].to_owned();
    let mut length = None;
    let mut non_wasm = false;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or(std::io::ErrorKind::InvalidData)?;
        if !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || value.bytes().any(|byte| byte < 32 && byte != b'\t')
        {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some()
                || value.is_empty()
                || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            length = Some(value.parse().map_err(|_| std::io::ErrorKind::InvalidData)?);
        }
        if name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case("content-encoding")
        {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        if name.eq_ignore_ascii_case("x-falcone-runtime-mode") && value != "wasm" {
            non_wasm = true;
        }
        if name.eq_ignore_ascii_case("x-falcone-image") {
            non_wasm = true;
        }
    }
    Ok(Header {
        path,
        length: length.unwrap_or(0),
        non_wasm,
    })
}
fn remaining(deadline: Instant) -> std::io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(std::io::ErrorKind::TimedOut.into())
    } else {
        Ok(remaining)
    }
}
fn read_body(stream: &mut TcpStream, mut body: &mut [u8]) -> std::io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !body.is_empty() {
        stream.set_read_timeout(Some(remaining(deadline)?.min(Duration::from_secs(2))))?;
        match stream.read(body) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => body = &mut body[count..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
fn write(stream: &mut TcpStream, status: u16, value: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(value)?;
    let header = format!(
        "HTTP/1.1 {status} Response\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    // One deadline for the header AND body, across all partial writes and EINTR.
    // write_all with a socket timeout would restart the allowance on each retry.
    let deadline = Instant::now() + Duration::from_secs(5);
    for mut bytes in [header.as_bytes(), body.as_slice()] {
        while !bytes.is_empty() {
            stream.set_write_timeout(Some(remaining(deadline)?))?;
            match stream.write(bytes) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(count) => bytes = &bytes[count..],
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}
