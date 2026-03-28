use sqry_lang_servicenow_xml::metadata::{RecordMetadata, sanitize_record_name, synthetic_path};
use std::path::Path;

#[test]
fn test_sanitize_spaces() {
    assert_eq!(
        sanitize_record_name("Validate Incident Priority"),
        "Validate_Incident_Priority"
    );
}

#[test]
fn test_sanitize_path_traversal() {
    let result = sanitize_record_name("../../etc/passwd");
    assert!(!result.contains('/'));
    assert!(!result.contains(".."));
}

#[test]
fn test_sanitize_slashes() {
    assert_eq!(
        sanitize_record_name("name/with\\slashes"),
        "name_with_slashes"
    );
}

#[test]
fn test_sanitize_empty() {
    assert_eq!(sanitize_record_name(""), "unnamed");
}

#[test]
fn test_sanitize_only_dots() {
    assert_eq!(sanitize_record_name("..."), "unnamed");
}

#[test]
fn test_sanitize_unicode_preserved() {
    let result = sanitize_record_name("\u{540d}\u{524d}");
    assert_eq!(result, "\u{540d}\u{524d}");
}

#[test]
fn test_sanitize_truncation() {
    let long_name = "a".repeat(300);
    assert_eq!(sanitize_record_name(&long_name).len(), 200);
}

#[test]
fn test_sanitize_special_chars() {
    assert_eq!(sanitize_record_name("file:name*test?"), "file_name_test_");
}

#[test]
fn test_synthetic_path_single_record() {
    let metadata = RecordMetadata {
        name: "TaskUtils".to_string(),
        table: "sys_script_include".to_string(),
        scope: "Global".to_string(),
        collection: String::new(),
    };
    let path = synthetic_path(
        Path::new("/export/sys_script_include_abc.xml"),
        &metadata,
        0,
        false,
    );
    // xml_stem "sys_script_include_abc" ++ "__" ++ table.name
    assert_eq!(
        path,
        Path::new("/export/sys_script_include_abc__sys_script_include.TaskUtils.snjs")
    );
}

#[test]
fn test_synthetic_path_multi_record() {
    let metadata = RecordMetadata {
        name: "TaskUtils".to_string(),
        table: "sys_script".to_string(),
        scope: String::new(),
        collection: String::new(),
    };
    let path = synthetic_path(Path::new("/export/multi.xml"), &metadata, 2, true);
    assert_eq!(
        path,
        Path::new("/export/multi__sys_script.TaskUtils_2.snjs")
    );
}

#[test]
fn test_synthetic_path_empty_name_uses_xml_stem() {
    let metadata = RecordMetadata {
        name: String::new(),
        table: "sys_script".to_string(),
        scope: String::new(),
        collection: String::new(),
    };
    let path = synthetic_path(
        Path::new("/export/sys_script_abc123.xml"),
        &metadata,
        0,
        false,
    );
    // empty name falls back to sanitized xml stem as the name part
    assert_eq!(
        path,
        Path::new("/export/sys_script_abc123__sys_script.sys_script_abc123.snjs")
    );
}

#[test]
fn test_synthetic_path_sanitizes_name() {
    let metadata = RecordMetadata {
        name: "My Script/Include".to_string(),
        table: "sys_ui_action".to_string(),
        scope: String::new(),
        collection: String::new(),
    };
    let path = synthetic_path(Path::new("/export/file.xml"), &metadata, 0, false);
    assert_eq!(
        path,
        Path::new("/export/file__sys_ui_action.My_Script_Include.snjs")
    );
}
