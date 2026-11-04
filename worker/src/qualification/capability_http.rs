use crate::host::{self, Bindings, Context, Grants, Host, HostError, Operation, Pair, Reply};
use host::net::{PinnedRequest, Response};
use std::{
    collections::VecDeque,
    net::IpAddr,
    sync::{Arc, Mutex},
};

struct Script {
    answers: Mutex<VecDeque<Vec<IpAddr>>>,
    responses: Mutex<VecDeque<Response>>,
    connected: Mutex<Vec<IpAddr>>,
}
impl Bindings for Script {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        panic!("HTTP must use pinned transport");
    }
    fn resolve(&self, _: &str) -> Result<Vec<IpAddr>, HostError> {
        Ok(self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected re-resolution"))
    }
    fn http(&self, _: &Context, request: &PinnedRequest) -> Result<Response, HostError> {
        self.connected.lock().unwrap().push(request.address().ip());
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected connection"))
    }
}
fn response(location: Option<&str>) -> Response {
    Response {
        status: if location.is_some() { 302 } else { 200 },
        headers: location
            .into_iter()
            .map(|value| Pair {
                name: "location".into(),
                value: value.into(),
            })
            .collect(),
        body: vec![42, 43],
    }
}
fn invoke(
    mode: u32,
    answers: Vec<Vec<&str>>,
    responses: Vec<Response>,
    extra_hosts: Vec<&str>,
) -> (u32, Arc<Script>) {
    let binding = Arc::new(Script {
        answers: Mutex::new(
            answers
                .into_iter()
                .map(|addresses| {
                    addresses
                        .into_iter()
                        .map(|ip| ip.parse().unwrap())
                        .collect()
                })
                .collect(),
        ),
        responses: Mutex::new(responses.into()),
        connected: Mutex::new(Vec::new()),
    });
    let mut hosts = vec!["allowed.test".into()];
    hosts.extend(extra_hosts.into_iter().map(str::to_owned));
    let context = Context {
        grants: Grants {
            hosts,
            ..Default::default()
        },
        ..Default::default()
    };
    let engine = wasmtime::Engine::default();
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("fixtures/capability-http.wat"),
        Host::new(context, binding.clone()),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    (run.call(&mut store, (mode,)).unwrap().0, binding)
}

#[cfg_attr(test, test)]
pub fn allow_list_resolution_pinning_and_every_redirect_are_enforced() {
    let _case = crate::capability_support::serial_case();
    let (result, binding) = invoke(
        0,
        vec![vec!["8.8.8.8"], vec!["127.0.0.1"]],
        vec![response(None)],
        vec![],
    );
    assert_eq!(result, 0);
    assert_eq!(
        *binding.connected.lock().unwrap(),
        ["8.8.8.8".parse::<IpAddr>().unwrap()]
    );
    assert_eq!(
        binding.answers.lock().unwrap().len(),
        1,
        "transport must not re-resolve"
    );
    let (result, binding) = invoke(1, vec![], vec![], vec![]);
    assert_eq!(result, 1);
    assert!(binding.connected.lock().unwrap().is_empty());
    for location in ["http://unlisted.test/", "http://allowed.test/redirect"] {
        let (result, binding) = invoke(
            0,
            vec![vec!["8.8.8.8"], vec!["127.0.0.1"]],
            vec![response(Some(location))],
            vec![],
        );
        assert_eq!(result, 1);
        assert_eq!(binding.connected.lock().unwrap().len(), 1);
    }
    let (result, binding) = invoke(
        0,
        vec![vec!["8.8.8.8"]; 6],
        (0..6).map(|_| response(Some("/again"))).collect(),
        vec![],
    );
    assert_eq!(result, 2, "redirect hop limit");
    assert_eq!(binding.connected.lock().unwrap().len(), 6);
    assert!(binding.answers.lock().unwrap().is_empty());
}

#[cfg_attr(test, test)]
pub fn private_dns_and_nat64_answers_and_redirects_never_reach_transport() {
    let _case = crate::capability_support::serial_case();
    for address in [
        "127.0.0.1",
        "10.0.0.1",
        "172.16.0.1",
        "192.168.1.1",
        "169.254.169.254",
        "64:ff9b:1::a9fe:a9fe",
        "64:ff9b:1:ffff::1",
        "64:ff9b::7f00:1",
        "64:ff9b::a00:1",
        "64:ff9b::a9fe:a9fe",
    ] {
        let (result, binding) = invoke(0, vec![vec![address]], vec![], vec![]);
        assert_eq!(result, 1, "{address}");
        assert!(binding.connected.lock().unwrap().is_empty());
        let location = if address.contains(':') {
            format!("http://[{address}]/")
        } else {
            format!("http://{address}/")
        };
        let (result, binding) = invoke(
            0,
            vec![vec!["8.8.8.8"]],
            vec![response(Some(&location))],
            vec![address],
        );
        assert_eq!(result, 1, "redirect {address}");
        assert_eq!(binding.connected.lock().unwrap().len(), 1);
    }
    let (result, binding) = invoke(
        0,
        vec![vec!["64:ff9b::808:808"]],
        vec![response(None)],
        vec![],
    );
    assert_eq!(result, 0);
    assert_eq!(binding.connected.lock().unwrap().len(), 1);
}

#[cfg_attr(test, test)]
pub fn http_lists_scalars_fields_and_responses_obey_the_aggregate_ceiling() {
    let _case = crate::capability_support::serial_case();
    for mode in 2..7 {
        let (result, binding) = invoke(mode, vec![], vec![], vec![]);
        assert_eq!(result, 2, "mode {mode}");
        assert!(binding.connected.lock().unwrap().is_empty());
    }
    let mut oversized = response(None);
    oversized.body = vec![0; 1_048_577];
    assert_eq!(
        invoke(0, vec![vec!["8.8.8.8"]], vec![oversized], vec![]).0,
        2
    );
}

#[cfg_attr(test, test)]
pub fn socket_rebinding_and_redirects_accept_no_forbidden_connections() {
    let _case = crate::capability_support::serial_case();
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        time::Duration,
    };
    let allowed = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error)
            if error.kind() == std::io::ErrorKind::PermissionDenied
                && !std::env::args().any(|arg| arg == "--capability-gates") =>
        {
            println!("NOT RUN: EPERM (sandbox), PR CI runs them");
            return;
        }
        Err(error) => panic!("listener: {error}"),
    };
    let forbidden = TcpListener::bind("127.0.0.1:0").unwrap();
    forbidden.set_nonblocking(true).unwrap();
    let allowed_address = allowed.local_addr().unwrap();
    let forbidden_address = forbidden.local_addr().unwrap();
    struct SocketBinding {
        allowed: std::net::SocketAddr,
        redirect: String,
        answers: Mutex<VecDeque<IpAddr>>,
    }
    impl Bindings for SocketBinding {
        fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
            unreachable!()
        }
        fn resolve(&self, _: &str) -> Result<Vec<IpAddr>, HostError> {
            Ok(vec![self.answers.lock().unwrap().pop_front().unwrap()])
        }
        fn http(&self, _: &Context, request: &PinnedRequest) -> Result<Response, HostError> {
            // This system-boundary fixture routes the scripted public peer to
            // the allowed listener. A forbidden peer would connect to itself.
            let address = if request.address().ip() == "8.8.8.8".parse::<IpAddr>().unwrap() {
                self.allowed
            } else {
                request.address()
            };
            let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(1)).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            socket.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
            let mut bytes = [0; 128];
            socket.read_exact(&mut bytes[..1]).unwrap();
            Ok(response(Some(&self.redirect)))
        }
    }
    let listener = std::thread::spawn(move || {
        let (mut socket, _) = allowed.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut bytes = [0; 128];
        let _ = socket.read(&mut bytes).unwrap();
        socket.write_all(b"x").unwrap();
    });
    let binding = Arc::new(SocketBinding {
        allowed: allowed_address,
        redirect: format!("http://allowed.test:{}/again", forbidden_address.port()),
        answers: Mutex::new(["8.8.8.8".parse().unwrap(), forbidden_address.ip()].into()),
    });
    let context = Context {
        grants: Grants {
            hosts: vec!["allowed.test".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let engine = wasmtime::Engine::default();
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("fixtures/capability-http.wat"),
        Host::new(context, binding),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, (0,)).unwrap(), (1,));
    listener.join().unwrap();
    assert_eq!(
        forbidden.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[cfg_attr(test, test)]
pub fn redirects_preserve_tls_and_scope_guest_credentials_to_the_origin() {
    struct Redirect {
        location: &'static str,
        headers: Mutex<Vec<Vec<Pair>>>,
    }
    impl Bindings for Redirect {
        fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
            unreachable!()
        }
        fn resolve(&self, _: &str) -> Result<Vec<IpAddr>, HostError> {
            Ok(vec!["8.8.8.8".parse().unwrap()])
        }
        fn http(&self, _: &Context, request: &PinnedRequest) -> Result<Response, HostError> {
            let mut headers = self.headers.lock().unwrap();
            headers.push(request.headers().to_vec());
            Ok(response((headers.len() == 1).then_some(self.location)))
        }
    }
    let source = include_str!("fixtures/capability-http.wat")
        .replace("http://allowed.test/pathGET", "https://allowed.test/pathGET")
        .replace("(i32.const 24)", "(i32.const 25)")
        .replace("(i32.const 1048)", "(i32.const 1049)")
        .replace("(data (i32.const 1280)", "(data (i32.const 2048) \"AuthorizationtokenCookiesessionX-Traceok\")\n    (data (i32.const 1280)")
        .replace("(call $request", r#"
          (local.set $headers (i32.const 3))
          (i32.store (i32.const 4096) (i32.const 2048)) (i32.store (i32.const 4100) (i32.const 13))
          (i32.store (i32.const 4104) (i32.const 2061)) (i32.store (i32.const 4108) (i32.const 5))
          (i32.store (i32.const 4112) (i32.const 2066)) (i32.store (i32.const 4116) (i32.const 6))
          (i32.store (i32.const 4120) (i32.const 2072)) (i32.store (i32.const 4124) (i32.const 7))
          (i32.store (i32.const 4128) (i32.const 2079)) (i32.store (i32.const 4132) (i32.const 7))
          (i32.store (i32.const 4136) (i32.const 2086)) (i32.store (i32.const 4140) (i32.const 2))
          (call $request"#);
    let engine = wasmtime::Engine::default();
    for (location, expected, retained) in [
        ("http://allowed.test/again", 1, 0),
        ("https://other.test/again", 0, 1),
        ("https://allowed.test:444/again", 0, 1),
        ("https://allowed.test/again", 0, 3),
    ] {
        let binding = Arc::new(Redirect {
            location,
            headers: Mutex::new(Vec::new()),
        });
        let context = Context {
            grants: Grants {
                hosts: vec!["allowed.test".into(), "other.test".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let (mut store, instance) =
            host::instantiate(&engine, &source, Host::new(context, binding.clone())).unwrap();
        let run = instance
            .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
            .unwrap();
        assert_eq!(
            run.call(&mut store, (0,)).unwrap(),
            (expected,),
            "{location}"
        );
        let headers = binding.headers.lock().unwrap();
        assert_eq!(headers[0].len(), 3);
        if retained == 0 {
            assert_eq!(headers.len(), 1);
        } else {
            assert_eq!(headers.len(), 2);
            assert_eq!(headers[1].len(), retained);
            if retained == 1 {
                assert_eq!(
                    (&*headers[1][0].name, &*headers[1][0].value),
                    ("X-Trace", "ok")
                );
            }
        }
    }
}
