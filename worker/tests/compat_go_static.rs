use falcone_serverless_worker::adapters::compat_go::{File, check_tree};

#[test]
fn cgo_is_rejected_at_its_import_without_echoing_source_data() {
    let files = [File {
        name: "main.go",
        text: "package main\nimport \"C\"\n// synthetic-private-value\n",
    }];
    let report = check_tree(&files, &[]);
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].code, "WASM-NATIVE-LIBRARY");
    assert_eq!(report[0].source, 0);
    assert_eq!(report[0].file, "main.go");
    assert_eq!(report[0].line, 2);
    assert!(!format!("{report:?}").contains("synthetic-private-value"));
}

#[test]
fn unsupported_features_have_stable_codes_and_locations() {
    for (text, code, line) in [
        (
            "package main\nimport alias \"os/exec\"",
            "WASM-PROCESS-SPAWN",
            2,
        ),
        (
            "package main\n//go:generate arbitrary-private-command",
            "WASM-INSTALL-HOOK",
            2,
        ),
        (
            "package main\nimport \"example.invalid/gpu\"",
            "WASM-GPU",
            2,
        ),
        (
            "package main\nimport \"reflect\"",
            "WASM-UNSUPPORTED-API",
            2,
        ),
        (
            "package main\nimport \"net/http\"",
            "WASM-UNSUPPORTED-API",
            2,
        ),
        ("package main\nimport \"os\"", "WASM-UNSUPPORTED-API", 2),
        (
            "package main\nimport \"example.invalid/library\"",
            "WASM-PACKAGE-NOT-IN-CATALOGUE",
            2,
        ),
        (
            "package main\nfunc main() { go work() }",
            "WASM-PROCESS-SPAWN",
            2,
        ),
    ] {
        let report = check_tree(
            &[File {
                name: "main.go",
                text,
            }],
            &[],
        );
        assert_eq!(report.len(), 1, "{code}: {report:?}");
        assert_eq!(report[0].code, code);
        assert_eq!(report[0].source, 0);
        assert_eq!(report[0].line, line);
        assert!(!format!("{report:?}").contains("arbitrary-private-command"));
    }
}

#[test]
fn module_requirements_and_floating_toolchains_are_refused() {
    for (text, code, line) in [
        (
            "module profile.invalid/go\ngo 1.26.11\nrequire example.invalid/a v1.0.0",
            "WASM-PACKAGE-NOT-IN-CATALOGUE",
            3,
        ),
        (
            "module profile.invalid/go\ngo 1.26.11\nrequire (\n example.invalid/a v1.0.0\n)",
            "WASM-PACKAGE-NOT-IN-CATALOGUE",
            3,
        ),
        (
            "module profile.invalid/go\ngo 1.26",
            "WASM-UNSUPPORTED-API",
            2,
        ),
        (
            "module profile.invalid/go\ngo 1.26.11\nreplace example.invalid/a => ../local",
            "WASM-PACKAGE-NOT-IN-CATALOGUE",
            3,
        ),
    ] {
        let report = check_tree(
            &[File {
                name: "go.mod",
                text,
            }],
            &[],
        );
        assert_eq!(report.len(), 1, "{report:?}");
        assert_eq!((report[0].code, report[0].line), (code, line));
    }
}

#[test]
fn only_listed_host_interfaces_and_local_source_imports_are_eligible() {
    let mut files = vec![
        File {
            name: "go.mod",
            text: "module profile.invalid/go\ngo 1.26.11\n",
        },
        File {
            name: "main.go",
            text: "package main\nimport \"profile.invalid/go/bindings\"\n",
        },
        File {
            name: "bindings/abi.go",
            text: "package bindings\n//go:wasmimport falcone:host/events@0.2.0 publish\nfunc publish()\n",
        },
    ];
    assert!(check_tree(&files, &["falcone:host/events@0.2.0"]).is_empty());
    let report = check_tree(&files, &[]);
    assert_eq!(report.len(), 1);
    assert_eq!(
        (report[0].source, report[0].line, report[0].code),
        (2, 2, "WASM-UNSUPPORTED-API")
    );
    files.pop();
    assert_eq!(
        check_tree(&files, &[])[0].code,
        "WASM-PACKAGE-NOT-IN-CATALOGUE"
    );
}

#[test]
fn comments_literals_aliases_and_escapes_are_lexed_without_leaking_content() {
    let text = "package main\n/* import \"C\" */\nconst a = `\n//go:generate hidden\nimport \"os/exec\"\n`\nconst b = \"reflect\"\nimport (\n _ \"encoding/json\"\n alias `crypto/sha256`\n _ \"os/\\x65xec\"\n)\n";
    let report = check_tree(
        &[File {
            name: "main.go",
            text,
        }],
        &[],
    );
    assert_eq!(report.len(), 1, "{report:?}");
    assert_eq!((report[0].code, report[0].line), ("WASM-PROCESS-SPAWN", 11));
    for text in [
        "package main\n/*",
        "package main\nimport \"bad\\q\"",
        "package main\n//go:linkname private arbitrary",
    ] {
        let report = check_tree(
            &[File {
                name: "main.go",
                text,
            }],
            &[],
        );
        assert_eq!(report.len(), 1, "{report:?}");
        assert_eq!(report[0].code, "WASM-UNSUPPORTED-API");
    }
}

#[test]
fn nested_module_requirements_are_rejected_and_directive_strings_are_inert() {
    let files = [
        File {
            name: "go.mod",
            text: "module profile.invalid/go\ngo 1.26.11\n",
        },
        File {
            name: "nested/go.mod",
            text: "module example.invalid/nested\ngo 1.26.11\nrequire example.invalid/private v1.0.0\n",
        },
        File {
            name: "main.go",
            text: "package main\nconst a = \"//go:wasmimport private:host/secret@0.2.0 read\"\n",
        },
    ];
    let report = check_tree(&files, &[]);
    assert_eq!(report.len(), 1, "{report:?}");
    assert_eq!(report[0].code, "WASM-PACKAGE-NOT-IN-CATALOGUE");
    assert_eq!(report[0].file, "nested/go.mod");
    assert_eq!(report[0].line, 3);
}
