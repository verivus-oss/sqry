use sqry_lang_servicenow_xml::detection::{RecordType, fast_precheck};

#[test]
fn test_fast_precheck_servicenow_xml() {
    let xml = b"<?xml version=\"1.0\"?><record_update table=\"sys_script\">";
    assert!(fast_precheck(xml));
}

#[test]
fn test_fast_precheck_svg() {
    let xml = b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\">";
    assert!(!fast_precheck(xml));
}

#[test]
fn test_fast_precheck_maven_pom() {
    let xml = b"<?xml version=\"1.0\"?><project xmlns=\"http://maven.apache.org\">";
    assert!(!fast_precheck(xml));
}

#[test]
fn test_fast_precheck_empty() {
    assert!(!fast_precheck(b""));
}

#[test]
fn test_fast_precheck_short_content() {
    assert!(!fast_precheck(b"<a/>"));
}

#[test]
fn test_record_type_sys_script() {
    let rt = RecordType::from_table("sys_script");
    assert!(rt.is_some());
    assert!(rt.unwrap().is_script());
}

#[test]
fn test_record_type_sys_script_include() {
    let rt = RecordType::from_table("sys_script_include");
    assert!(rt.is_some());
    assert!(rt.unwrap().is_script());
}

#[test]
fn test_record_type_sys_script_client() {
    let rt = RecordType::from_table("sys_script_client");
    assert!(rt.is_some());
    assert!(rt.unwrap().is_script());
}

#[test]
fn test_record_type_sys_ui_action_dual_fields() {
    let rt = RecordType::from_table("sys_ui_action").unwrap();
    assert!(rt.is_script());
    assert_eq!(rt.script_fields().unwrap(), &["script", "client_script"]);
}

#[test]
fn test_record_type_sys_ui_policy() {
    let rt = RecordType::from_table("sys_ui_policy").unwrap();
    assert!(rt.is_script());
    assert_eq!(
        rt.script_fields().unwrap(),
        &["script_true", "script_false"]
    );
}

#[test]
fn test_record_type_sys_ws_operation() {
    let rt = RecordType::from_table("sys_ws_operation").unwrap();
    assert_eq!(rt.script_fields().unwrap(), &["operation_script"]);
}

#[test]
fn test_record_type_sys_processor() {
    let rt = RecordType::from_table("sys_processor").unwrap();
    assert_eq!(rt.script_fields().unwrap(), &["script"]);
}

#[test]
fn test_record_type_sys_dictionary() {
    let rt = RecordType::from_table("sys_dictionary");
    assert!(rt.is_some());
    assert!(rt.unwrap().is_table_schema());
}

#[test]
fn test_record_type_sys_db_object() {
    let rt = RecordType::from_table("sys_db_object");
    assert!(rt.is_some());
    assert!(rt.unwrap().is_table_definition());
}

#[test]
fn test_record_type_unknown_table() {
    assert!(RecordType::from_table("sys_unknown").is_none());
    assert!(RecordType::from_table("").is_none());
    assert!(RecordType::from_table("incident").is_none());
}
