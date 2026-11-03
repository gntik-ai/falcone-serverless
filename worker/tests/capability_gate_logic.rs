//! WAS-02.t4: pure-logic tables of the deny-by-default capability gate. No guest is
//! instantiated and no socket, process or cluster is touched.

use falcone_serverless_worker::host::HostError;
use falcone_serverless_worker::host::grant::{GrantSet, ImportVerdict};
use falcone_serverless_worker::host::http::{Destination, RedirectError, redirect_target};
use falcone_serverless_worker::host::limits::{
    HOST_CALL_TIMEOUT, HTTP_REQUEST_FRAME_BYTES, HTTP_RESPONSE_FRAME_BYTES, HostLimits,
    RESULT_FRAME_BYTES, pair_bytes, result_bytes, string_bytes,
};
use falcone_serverless_worker::host::net::{AddressClass, Reach, classify, reach};
use serde_json::{Value, json};
use std::net::IpAddr;
use std::time::Duration;

const STORAGE: &str = "falcone:host/storage@0.1.0";
const HTTP: &str = "falcone:host/http@0.1.0";
const EVENTS: &str = "falcone:host/events@0.1.0";
const WORKFLOWS: &str = "falcone:host/workflows@0.1.0";

fn grant(family: &str, interface: &str, field: &str, entries: &[&str]) -> Value {
    json!({"family": family, "interface": interface, "scope": {field: entries}})
}

fn all_grants() -> GrantSet {
    GrantSet::from_manifest(&json!([
        grant(
            "storage",
            STORAGE,
            "prefixes",
            &["tenants/t1/ws1", "shared/ro/"]
        ),
        grant(
            "http",
            HTTP,
            "hosts",
            &["api.example.test", "api.example.test:8443", "10.1.2.3:8080"]
        ),
        grant("event", EVENTS, "topics", &["orders.created"]),
        grant(
            "workflow",
            WORKFLOWS,
            "targets",
            &["wf-ingest", "wf-report"]
        ),
    ]))
    .unwrap()
}

// ---- grant shape ------------------------------------------------------------------------

#[test]
fn an_empty_grant_array_grants_nothing() {
    let grants = GrantSet::from_manifest(&json!([])).unwrap();
    assert_eq!(
        grants.authorize_storage_key("tenants/t1/ws1/a"),
        Err(HostError::Denied)
    );
    assert_eq!(
        grants.authorize_event_topic("orders.created"),
        Err(HostError::Denied)
    );
    assert_eq!(
        grants.authorize_workflow_target("wf-ingest"),
        Err(HostError::Denied)
    );
    assert!(!grants.covers_interface(STORAGE));
}

#[test]
fn malformed_grants_are_rejected_not_widened() {
    let bad = [
        json!({"family": "storage", "interface": STORAGE, "scope": {"prefixes": ["*"]}}),
        json!({"family": "storage", "interface": STORAGE, "scope": {"prefixes": []}}),
        json!({"family": "storage", "interface": STORAGE, "scope": {"prefixes": [""]}}),
        json!({"family": "storage", "interface": STORAGE, "scope": {"topics": ["a"]}}),
        json!({"family": "storage", "interface": STORAGE, "scope": {"prefixes": ["a"], "topics": ["a"]}}),
        json!({"family": "storage", "interface": HTTP, "scope": {"prefixes": ["a"]}}),
        json!({"family": "wasi", "interface": STORAGE, "scope": {"prefixes": ["a"]}}),
        json!({"family": "storage", "interface": STORAGE, "scope": {"prefixes": ["a"]}, "extra": 1}),
        json!({"family": "storage", "interface": STORAGE}),
        json!({"family": "storage", "interface": "wasi:filesystem/preopens@0.2.0", "scope": {"prefixes": ["a"]}}),
        json!({"family": "storage", "interface": STORAGE, "scope": {"prefixes": ["../x"]}}),
        json!({"family": "event", "interface": EVENTS, "scope": {"topics": ["*"]}}),
        json!("storage"),
    ];
    for grant in bad {
        assert!(
            GrantSet::from_manifest(&json!([grant.clone()])).is_err(),
            "{grant}"
        );
    }
    assert!(GrantSet::from_manifest(&json!({"family": "storage"})).is_err());
    // Optional reference fields exist on the contract but never widen scope.
    let with_ref = json!([{
        "family": "event", "interface": EVENTS, "scope": {"topics": ["a"]},
        "secretRef": {"name": "binding"}, "guestGrantId": "g-1"
    }]);
    assert!(GrantSet::from_manifest(&with_ref).is_ok());
}

// ---- import names -----------------------------------------------------------------------

#[test]
fn imports_are_granted_ungranted_baseline_or_undeclared() {
    let grants =
        GrantSet::from_manifest(&json!([grant("storage", STORAGE, "prefixes", &["p"])])).unwrap();
    let table = [
        (STORAGE, ImportVerdict::Granted),
        (HTTP, ImportVerdict::Ungranted),
        (EVENTS, ImportVerdict::Ungranted),
        (WORKFLOWS, ImportVerdict::Ungranted),
        ("falcone:host/logging@0.1.0", ImportVerdict::Baseline),
        ("falcone:host/cancellation@0.1.0", ImportVerdict::Baseline),
        ("falcone:host/progress@0.1.0", ImportVerdict::Baseline),
        (
            "falcone:host/invocation-context@0.1.0",
            ImportVerdict::Baseline,
        ),
        ("wasi:filesystem/preopens@0.2.0", ImportVerdict::Undeclared),
        ("wasi:filesystem/types@0.2.0", ImportVerdict::Undeclared),
        ("wasi:sockets/tcp@0.2.0", ImportVerdict::Undeclared),
        ("wasi:cli/environment@0.2.0", ImportVerdict::Undeclared),
        (
            "wasi:http/outgoing-handler@0.2.0",
            ImportVerdict::Undeclared,
        ),
        ("falcone:host/storage@0.2.0", ImportVerdict::Undeclared),
        ("falcone:host/secrets@0.1.0", ImportVerdict::Undeclared),
        ("falcone:host/storage", ImportVerdict::Undeclared),
        ("FALCONE:HOST/STORAGE@0.1.0", ImportVerdict::Undeclared),
        ("falcone:host/storage@0.1.0 ", ImportVerdict::Undeclared),
        ("", ImportVerdict::Undeclared),
    ];
    for (name, expected) in table {
        assert_eq!(grants.classify_import(name), expected, "{name:?}");
    }
}

// ---- storage ----------------------------------------------------------------------------

#[test]
fn storage_keys_must_sit_inside_a_granted_prefix() {
    let grants = all_grants();
    let allowed = [
        "tenants/t1/ws1",
        "tenants/t1/ws1/obj",
        "tenants/t1/ws1/a/b/c.bin",
        "shared/ro/file",
    ];
    for key in allowed {
        assert_eq!(grants.authorize_storage_key(key), Ok(()), "{key}");
    }
    let denied = [
        "",
        "tenants/t1/ws2/obj",
        "tenants/t2/ws1/obj",
        "tenants/t1/ws10/obj",
        "tenants/t1/ws1x",
        "tenants/t1",
        "/tenants/t1/ws1/obj",
        "tenants/t1/ws1/../ws2/obj",
        "tenants/t1/ws1/..",
        "tenants/t1/ws1/./obj",
        "tenants/t1/ws1//obj",
        "tenants/t1/ws1/obj/",
        "tenants/t1/ws1\\..\\ws2",
        "tenants/t1/ws1/%2e%2e/ws2/obj",
        "tenants/t1/ws1/%2E%2E%2Fws2",
        "tenants/t1/ws1/%5c..",
        "tenants/t1/ws1/obj\0",
        "tenants/t1/ws1/obj\n",
        "tenants/t1/ws1/\u{202e}obj",
        "shared",
        "shared/rw/x",
        "other",
    ];
    for key in denied {
        assert_eq!(
            grants.authorize_storage_key(key),
            Err(HostError::Denied),
            "{key:?}"
        );
    }
    let long = format!("tenants/t1/ws1/{}", "a".repeat(2000));
    assert_eq!(grants.authorize_storage_key(&long), Err(HostError::Denied));
}

// ---- events and workflows ---------------------------------------------------------------

#[test]
fn events_and_workflows_match_exact_entries_only() {
    let grants = all_grants();
    assert_eq!(grants.authorize_event_topic("orders.created"), Ok(()));
    for topic in [
        "orders.created.v2",
        "orders",
        "ORDERS.CREATED",
        "orders.created ",
        "*",
        "",
        "orders.*",
    ] {
        assert_eq!(
            grants.authorize_event_topic(topic),
            Err(HostError::Denied),
            "{topic:?}"
        );
    }
    for target in ["wf-ingest", "wf-report"] {
        assert_eq!(grants.authorize_workflow_target(target), Ok(()));
    }
    for target in ["wf-ingest2", "wf", "WF-INGEST", "wf-other", "", "*", "wf-*"] {
        assert_eq!(
            grants.authorize_workflow_target(target),
            Err(HostError::Denied),
            "{target:?}"
        );
    }
}

#[test]
fn a_family_grant_never_authorizes_another_family() {
    let grants = GrantSet::from_manifest(&json!([grant(
        "event",
        EVENTS,
        "topics",
        &["tenants/t1/ws1/x", "wf-ingest"]
    )]))
    .unwrap();
    assert_eq!(
        grants.authorize_storage_key("tenants/t1/ws1/x"),
        Err(HostError::Denied)
    );
    assert_eq!(
        grants.authorize_workflow_target("wf-ingest"),
        Err(HostError::Denied)
    );
    assert!(!grants.covers_interface(STORAGE));
}

// ---- HTTP destinations ------------------------------------------------------------------

#[test]
fn destinations_are_parsed_strictly() {
    let ok = Destination::parse("http://API.Example.test/a/b?q=1#frag").unwrap();
    assert_eq!(ok.host(), "api.example.test");
    assert_eq!(ok.port(), 80);
    assert_eq!(ok.path_and_query(), "/a/b?q=1");
    let ok = Destination::parse("http://api.example.test.:8443").unwrap();
    assert_eq!(
        (ok.host(), ok.port(), ok.path_and_query()),
        ("api.example.test", 8443, "/")
    );
    let ip = Destination::parse("http://10.1.2.3:8080/x").unwrap();
    assert_eq!(ip.literal(), Some("10.1.2.3".parse().unwrap()));
    let v6 = Destination::parse("http://[::1]:9/x").unwrap();
    assert_eq!(v6.literal(), Some("::1".parse().unwrap()));
    for bad in [
        "",
        "api.example.test/a",
        "ftp://api.example.test/",
        "https://api.example.test/",
        "http://",
        "http:///a",
        "http://user@api.example.test/",
        "http://user:pw@api.example.test/",
        "http://api.example.test:0/",
        "http://api.example.test:99999/",
        "http://api.example.test:80a/",
        "http://api example.test/",
        "http://api.example.test/a b",
        "http://api.example.test/\r\nHost: evil",
        "http://api.example.test\\@evil/",
        "http://api.example.test/\0",
        "http://[::1/",
        "http://0x7f.1/",
        "http://2130706433/",
        "http://127.1/",
        "http://017700000001/",
    ] {
        assert!(Destination::parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_http_allow_list_matches_host_and_port_exactly() {
    let grants = all_grants();
    let check = |url: &str| grants.authorize_http(&Destination::parse(url).unwrap());
    assert_eq!(check("http://api.example.test/"), Ok(false));
    assert_eq!(check("http://API.EXAMPLE.TEST:80/x"), Ok(false));
    assert_eq!(check("http://api.example.test:8443/"), Ok(false));
    // An IP literal entry is the only way to reach a non-public address.
    assert_eq!(check("http://10.1.2.3:8080/"), Ok(true));
    for url in [
        "http://api.example.test:9000/",
        "http://evil.api.example.test/",
        "http://api.example.test.evil.test/",
        "http://example.test/",
        "http://10.1.2.3/",
        "http://10.1.2.3:8081/",
        "http://10.1.2.4:8080/",
        "http://169.254.169.254/",
    ] {
        assert_eq!(check(url), Err(HostError::Denied), "{url}");
    }
    let none = GrantSet::from_manifest(&json!([])).unwrap();
    assert_eq!(
        none.authorize_http(&Destination::parse("http://api.example.test/").unwrap()),
        Err(HostError::Denied)
    );
}

#[test]
fn redirects_are_resolved_against_the_current_url_and_bounded() {
    let current = Destination::parse("http://api.example.test:8443/a/b?x=1").unwrap();
    let hop = |location: &str| redirect_target(&current, location).map(|d| d.to_string());
    assert_eq!(hop("/c").unwrap(), "http://api.example.test:8443/c");
    assert_eq!(
        hop("d?y=2").unwrap(),
        "http://api.example.test:8443/a/d?y=2"
    );
    assert_eq!(
        hop("http://other.test/z").unwrap(),
        "http://other.test:80/z"
    );
    assert_eq!(hop("//other.test:81/z").unwrap(), "http://other.test:81/z");
    assert_eq!(hop("http://127.0.0.1:9/").unwrap(), "http://127.0.0.1:9/");
    for bad in [
        "",
        "https://api.example.test/",
        "ftp://x/",
        "http://",
        "http://u@x/",
        "/a\r\nb",
    ] {
        assert!(
            matches!(redirect_target(&current, bad), Err(RedirectError::Invalid)),
            "{bad:?}"
        );
    }
}

// ---- address classification -------------------------------------------------------------

#[test]
fn addresses_are_classified_and_only_public_ones_are_open() {
    use AddressClass::*;
    let table: &[(&str, AddressClass, Reach)] = &[
        ("93.184.216.34", Public, Reach::Open),
        ("8.8.8.8", Public, Reach::Open),
        ("172.15.255.255", Public, Reach::Open),
        ("172.32.0.1", Public, Reach::Open),
        ("2606:2800:220:1::1", Public, Reach::Open),
        ("127.0.0.1", Loopback, Reach::GrantOnly),
        ("127.255.255.254", Loopback, Reach::GrantOnly),
        ("::1", Loopback, Reach::GrantOnly),
        ("10.0.0.1", Private, Reach::GrantOnly),
        ("172.16.0.1", Private, Reach::GrantOnly),
        ("172.31.255.255", Private, Reach::GrantOnly),
        ("192.168.1.1", Private, Reach::GrantOnly),
        ("100.64.0.1", Shared, Reach::GrantOnly),
        ("100.127.255.255", Shared, Reach::GrantOnly),
        ("fd12:3456::1", UniqueLocal, Reach::GrantOnly),
        ("fc00::1", UniqueLocal, Reach::GrantOnly),
        ("169.254.169.254", Metadata, Reach::Never),
        ("100.100.100.200", Metadata, Reach::Never),
        ("fd00:ec2::254", Metadata, Reach::Never),
        ("169.254.0.1", LinkLocal, Reach::Never),
        ("fe80::1", LinkLocal, Reach::Never),
        ("0.0.0.0", Unspecified, Reach::Never),
        ("0.1.2.3", Unspecified, Reach::Never),
        ("::", Unspecified, Reach::Never),
        ("224.0.0.1", Multicast, Reach::Never),
        ("ff02::1", Multicast, Reach::Never),
        ("255.255.255.255", Broadcast, Reach::Never),
        ("240.0.0.1", Reserved, Reach::Never),
        ("192.0.2.1", Reserved, Reach::Never),
        ("198.18.0.1", Reserved, Reach::Never),
        ("2001:db8::1", Reserved, Reach::Never),
        // Embedded IPv4 follows the embedded address, never the wrapper.
        ("::ffff:127.0.0.1", Loopback, Reach::GrantOnly),
        ("::ffff:10.0.0.1", Private, Reach::GrantOnly),
        ("::ffff:169.254.169.254", Metadata, Reach::Never),
        ("::ffff:8.8.8.8", Public, Reach::Open),
        ("64:ff9b::7f00:1", Loopback, Reach::GrantOnly),
        ("64:ff9b::a9fe:a9fe", Metadata, Reach::Never),
        ("2002:7f00:1::1", Loopback, Reach::GrantOnly),
        ("2002:a9fe:a9fe::1", Metadata, Reach::Never),
        ("::7f00:1", Loopback, Reach::GrantOnly),
        (
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            Reserved,
            Reach::Never,
        ),
    ];
    for (text, class, expected) in table {
        let ip: IpAddr = text.parse().unwrap();
        assert_eq!(classify(ip), *class, "{text}");
        assert_eq!(reach(*class), *expected, "{text}");
    }
}

// ---- limits -----------------------------------------------------------------------------

#[test]
fn limits_match_the_wit_contract() {
    // The independent source of truth is the WIT doc comment, not the Rust constants.
    let wit = include_str!("../wit/host.wit");
    let value = |key: &str| -> u64 {
        let lines: Vec<u64> = wit
            .lines()
            .filter_map(|line| line.trim_start_matches('/').trim().strip_prefix(key))
            .map(|rest| rest.trim_start_matches(':').trim().parse().unwrap())
            .collect();
        assert_eq!(lines.len(), 8, "{key}: one per interface");
        assert!(lines.windows(2).all(|pair| pair[0] == pair[1]));
        lines[0]
    };
    let limits = HostLimits::default();
    assert_eq!(
        limits.guest_to_host_bytes() as u64,
        value("max-guest-to-host-bytes")
    );
    assert_eq!(
        limits.host_to_guest_bytes() as u64,
        value("max-host-to-guest-bytes")
    );
    assert_eq!(
        limits.call_timeout(),
        Duration::from_millis(value("host-call-timeout-ms"))
    );
    assert_eq!(HOST_CALL_TIMEOUT, Duration::from_secs(30));
}

#[test]
fn request_size_arithmetic_has_exact_edges_and_never_wraps() {
    let limits = HostLimits::default();
    let max = 1_048_576;
    assert_eq!(limits.check_request(&[]), Ok(()));
    assert_eq!(limits.check_request(&[max]), Ok(()));
    assert_eq!(
        limits.check_request(&[max + 1]),
        Err(HostError::LimitExceeded)
    );
    assert_eq!(limits.check_request(&[max - 10, 10]), Ok(()));
    assert_eq!(
        limits.check_request(&[max - 10, 11]),
        Err(HostError::LimitExceeded)
    );
    assert_eq!(
        limits.check_request(&[usize::MAX, 2]),
        Err(HostError::LimitExceeded)
    );
    assert_eq!(limits.check_response(max), Ok(()));
    assert_eq!(
        limits.check_response(max + 1),
        Err(HostError::LimitExceeded)
    );
    let tight = HostLimits::new(4, 8, Duration::from_millis(5));
    assert_eq!(tight.check_request(&[2, 2]), Ok(()));
    assert_eq!(tight.check_request(&[2, 3]), Err(HostError::LimitExceeded));
    assert_eq!(tight.check_response(8), Ok(()));
    assert_eq!(tight.check_response(9), Err(HostError::LimitExceeded));
}

#[test]
fn serialized_sizes_count_frames_and_records_on_top_of_contents() {
    // A string or list is a pointer and a length; a name/value record is two such pairs.
    assert_eq!(string_bytes(0), 8);
    assert_eq!(string_bytes(10), 18);
    assert_eq!(pair_bytes(0, 0), 16);
    assert_eq!(pair_bytes(1, 2), 19);
    // url, method, headers and body pairs; the result discriminator, status, headers and body
    // of a response.
    assert_eq!(HTTP_REQUEST_FRAME_BYTES, 32);
    assert_eq!(HTTP_RESPONSE_FRAME_BYTES, 24);
    // A returned `result<list<u8>, host-error>` is a discriminator and a pointer/length pair.
    assert_eq!(RESULT_FRAME_BYTES, 4);
    assert_eq!(result_bytes(0), 12);
    assert_eq!(result_bytes(10), 22);
    assert_eq!(result_bytes(usize::MAX), usize::MAX);
    let limits = HostLimits::default();
    assert_eq!(limits.check_response(result_bytes(1_048_564)), Ok(()));
    assert_eq!(
        limits.check_response(result_bytes(1_048_565)),
        Err(HostError::LimitExceeded)
    );
    assert_eq!(string_bytes(usize::MAX), usize::MAX);
    assert_eq!(pair_bytes(usize::MAX, 1), usize::MAX);
    // 65537 records alone are over the 1 MiB ceiling, whatever their contents.
    assert_eq!(
        limits.check_request(&[65_537 * pair_bytes(0, 0)]),
        Err(HostError::LimitExceeded)
    );
    assert_eq!(limits.check_request(&[65_536 * pair_bytes(0, 0)]), Ok(()));
}
