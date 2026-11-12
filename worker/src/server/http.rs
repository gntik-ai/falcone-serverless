//! Deliberately small HTTP/1.1 boundary: one JSON POST, fixed framing, always close.
use super::error;
use serde_json::Value;
use std::io::{Read, Write};

const MAX_HEADERS: usize = 16 * 1024;
const MAX_REQUEST: usize = 8 * 1024 * 1024;

pub(super) fn head(io: &mut impl Read) -> Result<(String, usize), (u16, Value)> {
    let bad = || error(400, "invalid_request");
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() == MAX_HEADERS {
            return Err(bad());
        }
        let mut byte = [0];
        io.read_exact(&mut byte).map_err(|_| bad())?;
        head.push(byte[0]);
    }
    let text = std::str::from_utf8(&head).map_err(|_| bad())?;
    let mut lines = text.split("\r\n");
    let mut line = lines.next().ok_or_else(bad)?.split(' ');
    if line.next() != Some("POST") {
        return Err(bad());
    }
    let path = line.next().ok_or_else(bad)?.to_owned();
    if line.next() != Some("HTTP/1.1") || line.next().is_some() {
        return Err(bad());
    }
    let mut length = None;
    let mut json = false;
    for line in lines.filter(|l| !l.is_empty()) {
        let (key, value) = line.split_once(':').ok_or_else(bad)?;
        if key.eq_ignore_ascii_case("transfer-encoding") {
            return Err(bad());
        }
        if key.eq_ignore_ascii_case("content-length") {
            if length.is_some() || !value.trim().bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad());
            }
            length = Some(value.trim().parse::<usize>().map_err(|_| bad())?);
        }
        if key.eq_ignore_ascii_case("content-type") {
            json = value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json");
        }
    }
    if !json {
        return Err(bad());
    }
    let length = length.ok_or_else(bad)?;
    if length > MAX_REQUEST {
        return Err(error(413, "request_too_large"));
    }
    Ok((path, length))
}

pub(super) fn write(io: &mut impl Write, (status, body): (u16, Value)) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&body)?;
    write!(
        io,
        "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    )?;
    io.write_all(&bytes)
}

pub(super) fn body(io: &mut impl Read, length: usize) -> Result<Vec<u8>, (u16, Value)> {
    let mut body = vec![0; length];
    io.read_exact(&mut body)
        .map_err(|_| error(400, "invalid_request"))?;
    Ok(body)
}
