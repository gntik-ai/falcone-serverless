//! WAS-02.t4: the egress client's HTTP handling against raw local listeners, without a guest.
//! Sockets are loopback only; a sandbox that forbids them (EPERM) reports NOT RUN.

use falcone_serverless_worker::host::HostError;
use falcone_serverless_worker::host::bindings::{CallControl, CancelToken};
use falcone_serverless_worker::host::egress::{HttpRequest, Resolver, TcpConnector, fetch};
use falcone_serverless_worker::host::grant::GrantSet;
use falcone_serverless_worker::host::limits::HostLimits;
use serde_json::json;
use std::io::{self, Read, Write};
use std::net::{IpAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

struct NoNames;

impl Resolver for NoNames {
    fn resolve(&self, _: &str) -> io::Result<Vec<IpAddr>> {
        Err(io::ErrorKind::NotFound.into())
    }
}

/// Serves each connection with the next canned response and records the request heads.
fn serve(responses: Vec<String>) -> Option<(u16, Arc<Mutex<Vec<String>>>)> {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            let _ = writeln!(io::stderr(), "NOT RUN: EPERM (sandbox), PR CI runs them");
            return None;
        }
        Err(error) => panic!("bind: {error}"),
    };
    let port = listener.local_addr().unwrap().port();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&heads);
    thread::spawn(move || {
        for response in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
                head.push(byte[0]);
            }
            recorded
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&head).into_owned());
            let _ = stream.write_all(response.as_bytes());
        }
    });
    Some((port, heads))
}

type Reply = Result<falcone_serverless_worker::host::egress::HttpResponse, HostError>;

/// Fetches from `port`, with every port in `granted` allow-listed on the loopback literal.
fn fetch_from(port: u16, granted: &[u16], extra: &[(&str, &str)], method: &str) -> Reply {
    fetch_limited(port, granted, extra, method, &HostLimits::default())
}

fn fetch_limited(
    port: u16,
    granted: &[u16],
    extra: &[(&str, &str)],
    method: &str,
    limits: &HostLimits,
) -> Reply {
    let hosts: Vec<String> = granted.iter().map(|p| format!("127.0.0.1:{p}")).collect();
    let grants = GrantSet::from_manifest(&json!([{
        "family": "http", "interface": "falcone:host/http@0.1.0", "scope": {"hosts": hosts}
    }]))
    .unwrap();
    let control = CallControl::new(
        CancelToken::default(),
        Instant::now() + Duration::from_secs(10),
    );
    fetch(
        &NoNames,
        &TcpConnector,
        &grants,
        limits,
        HttpRequest {
            url: format!("http://127.0.0.1:{port}/start"),
            method: method.to_owned(),
            headers: extra
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect(),
            body: Vec::new(),
        },
        &control,
    )
}

fn get(port: u16, extra: &[(&str, &str)], method: &str) -> Reply {
    fetch_from(port, &[port], extra, method)
}

fn response(status: &str, headers: &str, body: &str) -> String {
    format!("HTTP/1.1 {status}\r\n{headers}Connection: close\r\n\r\n{body}")
}

#[test]
fn content_length_chunked_and_close_delimited_bodies_are_decoded() {
    let Some((port, _)) = serve(vec![
        response("200 OK", "Content-Length: 5\r\n", "hello"),
        response(
            "200 OK",
            "Transfer-Encoding: chunked\r\n",
            "3\r\nabc\r\n2;ext=1\r\nde\r\n0\r\n\r\n",
        ),
        response("200 OK", "", "until close"),
    ]) else {
        return;
    };
    assert_eq!(get(port, &[], "GET").unwrap().body, b"hello");
    assert_eq!(get(port, &[], "GET").unwrap().body, b"abcde");
    let last = get(port, &[], "GET").unwrap();
    assert_eq!(
        (last.status, last.body.as_slice()),
        (200, b"until close".as_slice())
    );
}

#[test]
fn head_and_no_content_responses_carry_no_body() {
    let Some((port, _)) = serve(vec![
        response("200 OK", "Content-Length: 99\r\n", ""),
        response("204 No Content", "", ""),
    ]) else {
        return;
    };
    assert!(get(port, &[], "HEAD").unwrap().body.is_empty());
    assert_eq!(get(port, &[], "GET").unwrap().status, 204);
}

#[test]
fn malformed_responses_are_denied_not_passed_through() {
    let Some((port, _)) = serve(vec![
        "garbage\r\n\r\n".to_owned(),
        response("200 OK", "Content-Length: 5\r\n", "abc"),
        response(
            "200 OK",
            "Transfer-Encoding: chunked\r\n",
            "zz\r\nabc\r\n0\r\n\r\n",
        ),
        response("200 OK", "Content-Length: nope\r\n", ""),
    ]) else {
        return;
    };
    for _ in 0..4 {
        assert_eq!(get(port, &[], "GET").unwrap_err(), HostError::Denied);
    }
}

#[test]
fn a_redirect_to_an_ungranted_origin_is_never_contacted() {
    let Some((port, heads)) = serve(vec![
        response(
            "303 See Other",
            "Location: http://127.0.0.1:9/other\r\n",
            "",
        ),
        response("200 OK", "Content-Length: 2\r\n", "ok"),
    ]) else {
        return;
    };
    assert_eq!(
        get(port, &[("Authorization", "Bearer x")], "POST").unwrap_err(),
        HostError::Denied
    );
    let heads = heads.lock().unwrap();
    assert_eq!(heads.len(), 1, "the redirect target was never contacted");
    assert!(heads[0].starts_with("POST /start HTTP/1.1"));
}

#[test]
fn credentials_do_not_follow_a_redirect_to_another_granted_origin() {
    let Some((second, second_heads)) =
        serve(vec![response("200 OK", "Content-Length: 2\r\n", "ok")])
    else {
        return;
    };
    let Some((first, first_heads)) = serve(vec![response(
        "303 See Other",
        &format!("Location: http://127.0.0.1:{second}/next\r\n"),
        "",
    )]) else {
        return;
    };
    let headers = [
        ("Authorization", "Bearer x"),
        ("Cookie", "s=1"),
        ("X-Trace", "1"),
    ];
    assert_eq!(
        fetch_from(first, &[first, second], &headers, "POST")
            .unwrap()
            .body,
        b"ok"
    );
    let first_heads = first_heads.lock().unwrap();
    assert!(
        first_heads[0].contains("Authorization: Bearer x")
            && first_heads[0].contains("Cookie: s=1")
    );
    let second_heads = second_heads.lock().unwrap();
    assert!(
        second_heads[0].starts_with("GET /next HTTP/1.1"),
        "303 turns the POST into a GET"
    );
    assert!(!second_heads[0].contains("Authorization") && !second_heads[0].contains("Cookie"));
    assert!(second_heads[0].contains("X-Trace: 1"));
}

#[test]
fn same_origin_redirects_are_followed_with_a_rewritten_method_and_a_hop_limit() {
    let Some((port, heads)) = serve(vec![
        response("302 Found", "Location: /second\r\n", ""),
        response("200 OK", "Content-Length: 2\r\n", "ok"),
    ]) else {
        return;
    };
    let result = get(port, &[("X-Trace", "1")], "POST").unwrap();
    assert_eq!(result.body, b"ok");
    let heads = heads.lock().unwrap();
    assert!(heads[0].starts_with("POST /start HTTP/1.1"));
    assert!(heads[1].starts_with("GET /second HTTP/1.1"), "{}", heads[1]);
    assert!(heads[1].contains("X-Trace: 1"));
    let looping: Vec<String> = (0..8)
        .map(|_| response("302 Found", "Location: /start\r\n", ""))
        .collect();
    let Some((port, _)) = serve(looping) else {
        return;
    };
    assert_eq!(get(port, &[], "GET").unwrap_err(), HostError::Denied);
}

#[test]
fn guest_headers_cannot_inject_or_override_framing_and_methods_are_closed() {
    // No listener is needed: these are refused before any resolution or connection.
    let port = 9;
    for (name, value) in [
        ("Host", "evil.test"),
        ("Content-Length", "1"),
        ("Transfer-Encoding", "chunked"),
        ("Connection", "keep-alive"),
        ("X-A", "b\r\nX-Injected: 1"),
        ("X A", "b"),
        ("", "b"),
    ] {
        assert_eq!(
            get(port, &[(name, value)], "GET").unwrap_err(),
            HostError::Denied,
            "{name:?}"
        );
    }
    for method in ["TRACE", "CONNECT", "get", "GET /x HTTP/1.1\r\nX:", ""] {
        assert_eq!(
            get(port, &[], method).unwrap_err(),
            HostError::Denied,
            "{method:?}"
        );
    }
}

#[test]
fn a_cancelled_or_expired_call_never_connects() {
    let Some((port, heads)) = serve(vec![response("200 OK", "Content-Length: 0\r\n", "")]) else {
        return;
    };
    let grants = GrantSet::from_manifest(&json!([{
        "family": "http", "interface": "falcone:host/http@0.1.0",
        "scope": {"hosts": [format!("127.0.0.1:{port}")]}
    }]))
    .unwrap();
    let request = || HttpRequest {
        url: format!("http://127.0.0.1:{port}/"),
        method: "GET".into(),
        ..HttpRequest::default()
    };
    let cancel = CancelToken::default();
    cancel.cancel();
    let control = CallControl::new(cancel, Instant::now() + Duration::from_secs(5));
    let limits = HostLimits::default();
    assert_eq!(
        fetch(
            &NoNames,
            &TcpConnector,
            &grants,
            &limits,
            request(),
            &control
        )
        .unwrap_err(),
        HostError::Cancelled
    );
    let expired = CallControl::new(CancelToken::default(), Instant::now());
    assert_eq!(
        fetch(
            &NoNames,
            &TcpConnector,
            &grants,
            &limits,
            request(),
            &expired
        )
        .unwrap_err(),
        HostError::Timeout
    );
    assert!(heads.lock().unwrap().is_empty());
}

/// Wire bytes the adversarial servers stream; far above the 64 KiB ceiling the tests set.
const FLOOD: usize = 4 * 1024 * 1024;
const SMALL_CEILING: usize = 64 * 1024;

fn small_limits() -> HostLimits {
    HostLimits::new(SMALL_CEILING, SMALL_CEILING, Duration::from_secs(10))
}

#[test]
fn an_endless_chunk_extension_line_hits_the_byte_ceiling() {
    let flood = format!("5;{}", "a".repeat(FLOOD));
    let Some((port, _)) = serve(vec![response(
        "200 OK",
        "Transfer-Encoding: chunked\r\n",
        &flood,
    )]) else {
        return;
    };
    assert_eq!(
        fetch_limited(port, &[port], &[], "GET", &small_limits()).unwrap_err(),
        HostError::LimitExceeded
    );
}

#[test]
fn endless_chunk_trailers_hit_the_byte_ceiling() {
    let flood = format!("0\r\nX-Trailer: {}", "a".repeat(FLOOD));
    let Some((port, _)) = serve(vec![response(
        "200 OK",
        "Transfer-Encoding: chunked\r\n",
        &flood,
    )]) else {
        return;
    };
    assert_eq!(
        fetch_limited(port, &[port], &[], "GET", &small_limits()).unwrap_err(),
        HostError::LimitExceeded
    );
}

#[test]
fn a_chunk_larger_than_the_ceiling_is_rejected_from_its_size_line() {
    let Some((port, _)) = serve(vec![response(
        "200 OK",
        "Transfer-Encoding: chunked\r\n",
        &format!("{:x}\r\nabc", SMALL_CEILING * 2),
    )]) else {
        return;
    };
    assert_eq!(
        fetch_limited(port, &[port], &[], "GET", &small_limits()).unwrap_err(),
        HostError::LimitExceeded
    );
}

#[test]
fn many_small_chunks_and_trailers_within_the_ceiling_are_decoded() {
    let body: String = (0..2000).map(|_| "1\r\nx\r\n").collect();
    let Some((port, _)) = serve(vec![response(
        "200 OK",
        "Transfer-Encoding: chunked\r\n",
        &format!("{body}0\r\nX-Sum: 1\r\n\r\n"),
    )]) else {
        return;
    };
    assert_eq!(
        fetch_limited(port, &[port], &[], "GET", &small_limits())
            .unwrap()
            .body,
        vec![b'x'; 2000]
    );
}

#[test]
fn request_header_records_count_towards_the_guest_to_host_ceiling() {
    let Some((port, heads)) = serve(vec![response("200 OK", "Content-Length: 0\r\n", "")]) else {
        return;
    };
    // 4000 one-byte names with empty values are 4000 bytes of contents but 68000 serialized
    // bytes: each is a 16 byte record, so the 64 KiB ceiling is crossed.
    let many: Vec<(&str, &str)> = vec![("a", ""); 4000];
    assert_eq!(
        fetch_limited(port, &[port], &many, "GET", &small_limits()).unwrap_err(),
        HostError::LimitExceeded
    );
    assert!(heads.lock().unwrap().is_empty(), "nothing was sent");
    // 3000 of them are 51000 serialized bytes and go through.
    assert_eq!(
        fetch_limited(port, &[port], &many[..3000], "GET", &small_limits())
            .unwrap()
            .status,
        200
    );
}

#[test]
fn response_header_records_count_towards_the_host_to_guest_ceiling() {
    // 4000 headers of `X: 1` are 8000 bytes of contents but 72000 serialized bytes.
    let flood: String = (0..4000).map(|_| "X: 1\r\n").collect();
    let Some((port, _)) = serve(vec![response("200 OK", &flood, "")]) else {
        return;
    };
    assert_eq!(
        fetch_limited(port, &[port], &[], "GET", &small_limits()).unwrap_err(),
        HostError::LimitExceeded
    );
}
