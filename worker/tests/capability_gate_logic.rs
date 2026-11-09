use falcone_serverless_worker::host::{Grants, import_allowed};

#[test]
fn only_exact_world_interfaces_with_required_grants_are_linked() {
    let grants = Grants::default();
    for name in ["storage", "http", "events", "workflows"] {
        assert!(!import_allowed(
            &format!("falcone:host/{name}@0.2.0"),
            &grants
        ));
    }
    for name in ["logging", "cancellation", "progress", "invocation-context"] {
        assert!(import_allowed(
            &format!("falcone:host/{name}@0.2.0"),
            &grants
        ));
    }
    for name in [
        "wasi:filesystem/preopens@0.2.0",
        "wasi:sockets/tcp@0.2.0",
        "wasi:cli/environment@0.2.0",
        "falcone:host/storage@0.2.0",
    ] {
        assert!(!import_allowed(name, &grants));
    }
    let grants = Grants {
        prefixes: vec!["objects/".into()],
        hosts: vec!["example.com".into()],
        topics: vec!["orders".into()],
        targets: vec!["invoice".into()],
    };
    for name in ["storage", "http", "events", "workflows"] {
        assert!(import_allowed(
            &format!("falcone:host/{name}@0.2.0"),
            &grants
        ));
    }
}

#[test]
fn egress_classification_covers_nat64_and_its_prefix_boundaries() {
    use falcone_serverless_worker::host::net::public_address;
    for address in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.1",
        "192.168.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "::1",
        "fc00::1",
        "fe80::1",
        "::ffff:127.0.0.1",
        "64:ff9b:1::",
        "64:ff9b:1:ffff:ffff:ffff:ffff:ffff",
        "64:ff9b:1::a9fe:a9fe",
        "64:ff9b::7f00:1",
        "64:ff9b::a00:1",
        "64:ff9b::ac10:1",
        "64:ff9b::c0a8:101",
        "64:ff9b::a9fe:a9fe",
    ] {
        assert!(!public_address(address.parse().unwrap()), "{address}");
    }
    for address in [
        "8.8.8.8",
        "1.1.1.1",
        "2606:4700:4700::1111",
        "::ffff:8.8.8.8",
        "64:ff9b::808:808",
    ] {
        assert!(public_address(address.parse().unwrap()), "{address}");
    }
}

#[test]
fn storage_prefixes_have_path_boundaries_and_no_escape_syntax() {
    let grants = Grants {
        prefixes: vec!["objects/".into()],
        ..Default::default()
    };
    for key in ["objects/good", "objects/nested/good"] {
        assert!(grants.allows_key(key), "{key}");
    }
    for key in [
        "objects",
        "other/good",
        "objectsevil/good",
        "../objects/good",
        "objects/../other",
        "/objects/good",
        "objects//good",
        "objects/./good",
        "objects/%2e%2e/other",
        "objects\\other",
    ] {
        assert!(!grants.allows_key(key), "{key}");
    }
    let grants = Grants {
        prefixes: vec!["objects".into()],
        ..Default::default()
    };
    assert!(grants.allows_key("objects"));
    assert!(grants.allows_key("objects/good"));
    assert!(!grants.allows_key("objectsevil/good"));
}

#[test]
fn aggregate_limit_includes_all_parts_and_rejects_integer_overflow() {
    use falcone_serverless_worker::host::aggregate_within_limit;
    assert!(aggregate_within_limit(&[]));
    assert!(aggregate_within_limit(&[1_048_576]));
    assert!(aggregate_within_limit(&[1_048_575, 1]));
    assert!(!aggregate_within_limit(&[1_048_576, 1]));
    assert!(!aggregate_within_limit(&[usize::MAX, 1]));
}
