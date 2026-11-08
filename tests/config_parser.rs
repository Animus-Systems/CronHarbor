use cronharbor::config::{MAX_CONFIG_BYTES, Manifest, parse_yaml};

#[test]
fn collection_and_nested_alias_expansion_share_the_scalar_and_node_budgets() {
    let collection = format!(
        "[&a [{}], {}]",
        vec!["x".repeat(1024); 8].join(", "),
        vec!["*a"; 16].join(", ")
    );
    let mut nested = String::from("[&a [x, x], &b [*a, *a]");
    let mut previous = 'b';
    for anchor in 'c'..='s' {
        nested.push_str(&format!(", &{anchor} [*{previous}, *{previous}]"));
        previous = anchor;
    }
    nested.push(']');
    for source in [collection, nested] {
        assert!(source.len() < MAX_CONFIG_BYTES);
        let error = parse_yaml::<serde_json::Value>(source.as_bytes()).unwrap_err();
        assert_eq!(error.message, "Configuration exceeds YAML parsing limits");
    }
}

#[test]
fn deeply_retained_anchors_are_bounded_separately_from_original_scalar_bytes() {
    let mut source = (0..32)
        .map(|index| format!("&a{index} ["))
        .collect::<String>();
    source.push_str(&format!("\"{}\\n\"", "x".repeat(4096)));
    source.push_str(&"]".repeat(32));
    assert!(source.len() < MAX_CONFIG_BYTES);
    let error = parse_yaml::<serde_json::Value>(source.as_bytes()).unwrap_err();
    assert_eq!(error.message, "Configuration exceeds YAML parsing limits");
}

#[test]
fn parsing_caps_raw_bytes_and_depth_and_rejects_multiple_documents() {
    assert_eq!(
        parse_yaml::<serde_json::Value>(&vec![b' '; MAX_CONFIG_BYTES + 1])
            .unwrap_err()
            .message,
        "Configuration exceeds 64 KiB"
    );
    assert!(
        parse_yaml::<serde_json::Value>(
            format!("{}0{}", "[".repeat(65), "]".repeat(65)).as_bytes()
        )
        .is_err()
    );
    assert!(parse_yaml::<serde_json::Value>(b"---\n[]\n---\n[]").is_err());
    assert!(parse_yaml::<serde_json::Value>(&[0xff]).is_err());
}

#[test]
fn scalar_inference_preserves_explicit_types_and_string_spellings() {
    let value = parse_yaml::<serde_json::Value>(
        br#"
plain: 123
quoted: "123"
leading: 0012
underscored: 1_000
flag: true
"#,
    )
    .unwrap();
    assert_eq!(
        value,
        serde_json::json!({"plain":123,"quoted":"123","leading":"0012","underscored":"1_000","flag":true})
    );
    let source = include_str!("../examples/hello.yaml")
        .replace("[\"Hello from CronHarbor\"]", "['123', '456', 0012, 1_000]");
    let manifest = Manifest::parse(&source).unwrap();
    assert_eq!(
        manifest.spec.execution.args,
        ["123", "456", "0012", "1_000"]
    );
    assert_eq!(
        parse_yaml::<u64>(u64::MAX.to_string().as_bytes()).unwrap(),
        u64::MAX
    );
    assert_eq!(
        parse_yaml::<i64>(i64::MIN.to_string().as_bytes()).unwrap(),
        i64::MIN
    );
    assert!(parse_yaml::<serde_json::Value>(b".nan").is_err());
    assert!(parse_yaml::<serde_json::Value>(b".inf").is_err());
}

#[test]
fn typed_values_reject_quoted_primitives_and_unquoted_string_coercions() {
    let source = include_str!("../examples/hello.yaml");
    for (from, to) in [
        ("enabled: true", "enabled: 'true'"),
        ("enabled: true", "enabled: \"false\""),
        ("timeoutSeconds: 5", "timeoutSeconds: '5'"),
        ("timeoutSeconds: 5", "timeoutSeconds: 1_000"),
        ("timeoutSeconds: 5", "timeoutSeconds: 05"),
        ("maxAttempts: 1", "maxAttempts: '1'"),
        ("[\"Hello from CronHarbor\"]", "[123]"),
        ("[\"Hello from CronHarbor\"]", "[true]"),
        ("[\"Hello from CronHarbor\"]", "[0x10]"),
    ] {
        assert!(
            Manifest::parse(&source.replace(from, to)).is_err(),
            "accepted {to}"
        );
    }
    let aliased = source
        .replace(
            "  namespace: local",
            "  namespace: local\n  labels:\n    flag: &flag 'true'",
        )
        .replace("enabled: true", "enabled: *flag");
    assert!(Manifest::parse(&aliased).is_err());
}

#[test]
fn daemon_config_rejects_quoted_boolean_instead_of_coercing_it() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("daemon.yaml");
    std::fs::write(
        &config,
        format!(
            "paused: 'true'\nworkloads: ['{}']\n",
            directory.path().join("missing.yaml").display()
        ),
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cronharbor"))
        .arg("--state-dir")
        .arg(directory.path().join("state"))
        .args(["server", "--config"])
        .arg(config)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Configuration does not match the supported schema")
    );
    assert!(!directory.path().join("state").exists());
}

#[test]
fn duplicate_keys_are_rejected_in_structs_and_ordinary_maps() {
    let source = include_str!("../examples/hello.yaml");
    for (from, to) in [
        ("enabled: true", "enabled: true\n  enabled: false"),
        (
            "  namespace: local",
            "  namespace: local\n  labels:\n    duplicate: first\n    duplicate: last",
        ),
    ] {
        assert!(Manifest::parse(&source.replace(from, to)).is_err());
    }
    assert!(parse_yaml::<serde_json::Value>(b"key: first\nkey: last").is_err());
    for source in [
        "enabled: true\n!!str enabled: false",
        "enabled: true\n'enabled': false",
        "enabled: true\n\"enab\\u006ced\": false",
    ] {
        assert!(
            parse_yaml::<serde_json::Value>(source.as_bytes()).is_err(),
            "accepted {source}"
        );
    }
}

#[test]
fn custom_tags_cannot_change_key_identity_before_conversion_to_json() {
    for source in [
        "!x enabled: true\nenabled: false",
        "enabled: true\n!x enabled: false",
        "value: !x 123",
    ] {
        assert!(
            parse_yaml::<serde_json::Value>(source.as_bytes()).is_err(),
            "accepted {source}"
        );
    }
    assert_eq!(
        parse_yaml::<serde_json::Value>(b"text: !!str 123\nnumber: !!int '5'\nflag: !!bool 'true'")
            .unwrap(),
        serde_json::json!({"text":"123","number":5,"flag":true})
    );
}

#[test]
fn configuration_mapping_keys_must_be_strings_before_json_normalization() {
    for source in [
        "123: first\n'123': last",
        "!!int '123': first\n'123': last",
        "outer: {123: first, '123': last}",
        "outer: {!!int '123': first, '123': last}",
        "true: value",
        "null: value",
        "? [a, b]\n: value",
    ] {
        assert!(
            parse_yaml::<serde_json::Value>(source.as_bytes()).is_err(),
            "accepted {source}"
        );
    }
    assert_eq!(
        parse_yaml::<serde_json::Value>(b"'123': first\n!!str true: second").unwrap(),
        serde_json::json!({"123":"first","true":"second"})
    );
}

#[test]
fn secret_reference_internal_tags_and_canonical_roundtrips_remain_supported() {
    let source = include_str!("../examples/hello.yaml").replace("    sandbox: default", "    sandbox: default\n    secretRefs:\n      env_secret: {provider: env, name: CRONHARBOR_TEST_SECRET}\n      file_secret: {provider: file, path: /private/secret}");
    let manifest = Manifest::parse(&source).unwrap();
    assert!(
        matches!(&manifest.spec.execution.secret_refs["env_secret"], cronharbor::config::SecretRef::Env { name } if name == "CRONHARBOR_TEST_SECRET")
    );
    assert!(
        matches!(&manifest.spec.execution.secret_refs["file_secret"], cronharbor::config::SecretRef::File { path } if path == "/private/secret")
    );
    let canonical = manifest.canonical().unwrap().0;
    assert_eq!(
        Manifest::parse(&canonical).unwrap().canonical().unwrap().0,
        canonical
    );
}

#[test]
fn legacy_service_fields_normalize_to_canonical_defaults() {
    let source = include_str!("../examples/hello.yaml")
        .replace("kind: ScheduledJob", "kind: Service")
        .replace("  retry:", "  restart:\n    policy: on-failure\n    maxAttempts: 8\n    windowSeconds: 300\n  shutdown:\n    graceSeconds: 30\n  retry:");
    let manifest = Manifest::parse(&source).unwrap();
    assert_eq!(manifest.spec.service.restart, "on-failure");
    assert_eq!(manifest.spec.service.max_restarts, 8);
    assert_eq!(manifest.spec.service.window_seconds, 300);
    assert_eq!(manifest.spec.service.stop_grace_seconds, 30);
    assert!(manifest.spec.restart.is_none());
    assert!(manifest.spec.shutdown.is_none());
    let (canonical, _) = manifest.canonical().unwrap();
    assert_eq!(
        Manifest::parse(&canonical).unwrap().canonical().unwrap().0,
        canonical
    );
}

#[test]
fn alias_expansion_is_rejected_by_the_parser_before_owned_schema_validation() {
    let args = format!(
        "[&value {}, {}]",
        "x".repeat(4096),
        vec!["*value"; 63].join(", ")
    );
    let source =
        include_str!("../examples/hello.yaml").replace("[\"Hello from CronHarbor\"]", &args);
    assert!(source.len() < MAX_CONFIG_BYTES);
    let error = Manifest::parse(&source).unwrap_err();
    assert_eq!(error.code, "CONFIG_INVALID");
    assert_eq!(error.message, "Configuration exceeds YAML parsing limits");
    assert!(!error.message.contains("xxxx"));
}

#[test]
fn daemon_config_uses_the_same_pre_deserialization_alias_budget() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("daemon.yaml");
    let source = format!(
        "workloads: [&path {}, {}]\n",
        "x".repeat(4096),
        vec!["*path"; 63].join(", ")
    );
    assert!(source.len() < MAX_CONFIG_BYTES);
    std::fs::write(&config, source).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cronharbor"))
        .arg("--state-dir")
        .arg(directory.path().join("state"))
        .args(["server", "--config"])
        .arg(config)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Configuration exceeds YAML parsing limits")
    );
    assert!(!directory.path().join("state").exists());
}

#[test]
fn ordinary_aliases_and_literal_yaml_content_keep_their_meaning() {
    let args = format!("[&value 'héllo * &', {}]", vec!["*value"; 150].join(", "));
    let source = include_str!("../examples/hello.yaml")
        .replace("[\"Hello from CronHarbor\"]", &args)
        .replace("  namespace: local", "  namespace: local\n  labels:\n    <<: literal\n    code: 0012\n    underscored: 1_000")
        .replace("    sandbox: default", "    sandbox: default\n    env:\n      text: |\n        Unicode 🌊 and quoted * & ${UNCHANGED}");
    let manifest = Manifest::parse(&source).unwrap();
    assert_eq!(manifest.spec.execution.args, vec!["héllo * &"; 151]);
    assert_eq!(manifest.metadata.labels["<<"], "literal");
    assert_eq!(manifest.metadata.labels["code"], "0012");
    assert_eq!(manifest.metadata.labels["underscored"], "1_000");
    assert_eq!(
        manifest.spec.execution.env["text"],
        "Unicode 🌊 and quoted * & ${UNCHANGED}\n"
    );
    let (canonical, _) = manifest.canonical().unwrap();
    assert_eq!(
        Manifest::parse(&canonical).unwrap().canonical().unwrap().0,
        canonical
    );
}

#[test]
fn yaml_type_and_duplicate_errors_remain_safe_and_strict() {
    let source = include_str!("../examples/hello.yaml");
    for replacement in [
        "enabled: yes",
        "enabled: on",
        "enabled: true\n  enabled: false",
    ] {
        let error = Manifest::parse(&source.replace("enabled: true", replacement)).unwrap_err();
        assert_eq!(error.code, "CONFIG_INVALID");
        assert!(!error.message.contains(replacement));
    }
    for argument in ["'null'", "'~'", "'*literal'", "'&literal'"] {
        assert!(
            Manifest::parse(
                &source.replace("[\"Hello from CronHarbor\"]", &format!("[{argument}]"))
            )
            .is_ok()
        );
    }
    for argument in ["null", "~"] {
        assert!(
            Manifest::parse(
                &source.replace("[\"Hello from CronHarbor\"]", &format!("[{argument}]"))
            )
            .is_err()
        );
    }
    assert!(
        Manifest::parse(&source.replace("args: [\"Hello from CronHarbor\"]", "args:")).is_err()
    );
}
