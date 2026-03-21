use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::json;
use tower_lsp::lsp_types::{
    CallHierarchyIncomingCallsParams, CallHierarchyItem, CallHierarchyOutgoingCallsParams,
    CallHierarchyPrepareParams, Position, TextDocumentIdentifier, TextDocumentPositionParams, Url,
    WorkDoneProgressParams,
};

use super::common;

fn new_session(root: &Path) -> sqry_lsp::session::SessionManager {
    common::ensure_index(root).expect("index build");
    let options = common::options_for(root);
    sqry_lsp::session::SessionManager::new(options)
}

fn parse_item_data(item: &CallHierarchyItem) -> TestCallHierarchyData {
    serde_json::from_value(item.data.clone().expect("call hierarchy data"))
        .expect("deserialize data")
}

#[derive(Debug, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum TestCallHierarchyData {
    Saved {
        qualified_name: String,
        file_path: PathBuf,
        #[serde(default)]
        _language: Option<String>,
    },
    Unsaved {
        message: String,
    },
}

#[test]
fn prepare_returns_saved_item_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let path =
        std::fs::canonicalize(root.join("src/lib.rs")).unwrap_or_else(|_| root.join("src/lib.rs"));

    let params = CallHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&path).unwrap(),
            },
            position: Position::new(40, 5), // summarize
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };

    let result = sqry_lsp::handlers::call_hierarchy::prepare(&session, &params)?
        .expect("call hierarchy item");
    assert_eq!(result.len(), 1);
    let item = &result[0];
    assert_eq!(item.name, "summarize");
    match parse_item_data(item) {
        TestCallHierarchyData::Saved {
            qualified_name,
            file_path,
            ..
        } => {
            assert!(qualified_name.ends_with("summarize"));
            assert_eq!(file_path, path);
        }
        TestCallHierarchyData::Unsaved { .. } => bail!("unexpected unsaved state"),
    }
    Ok(())
}

#[test]
fn incoming_calls_return_caller_ranges_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let path =
        std::fs::canonicalize(root.join("src/lib.rs")).unwrap_or_else(|_| root.join("src/lib.rs"));

    let prepare_params = CallHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&path).unwrap(),
            },
            position: Position::new(40, 5),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let items = sqry_lsp::handlers::call_hierarchy::prepare(&session, &prepare_params)?
        .expect("call hierarchy item");
    let item = items.into_iter().next().unwrap();

    let incoming_params = CallHierarchyIncomingCallsParams {
        item: item.clone(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: Default::default(),
    };

    let response = sqry_lsp::handlers::call_hierarchy::incoming(&session, &incoming_params)?;
    assert!(!response.items.is_empty());
    let caller = &response.items[0];
    assert!(
        [
            "orchestrate", // orchestrate calls summarize twice
            "alternate"
        ]
        .contains(&caller.from.name.as_str())
    );
    assert!(!caller.from_ranges.is_empty());
    match parse_item_data(&caller.from) {
        TestCallHierarchyData::Saved { file_path, .. } => assert_eq!(file_path, path),
        _ => bail!("expected saved data"),
    }
    Ok(())
}

/// Test cross-file incoming calls detection.
///
/// This test verifies that when function A in file1 calls function B in file2,
/// the call hierarchy correctly identifies A as a caller of B.
///
/// # Implementation
///
/// The Rust plugin qualifies symbols with their file-level module path
/// (e.g., `extra::helper` for `helper` in `src/extra.rs`). When a call is made
/// to a cross-file symbol, `ensure_function()` creates a local stub node.
///
/// Pass 4's `resolve_unresolved_ref` uses `ExportMap::lookup_cross_file()` to
/// find the real definition in a different file than the stub, creating the
/// correct cross-file call edge.
#[test]
fn incoming_calls_include_cross_file_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let extra_path = std::fs::canonicalize(root.join("src/extra.rs"))
        .unwrap_or_else(|_| root.join("src/extra.rs"));
    let lib_path =
        std::fs::canonicalize(root.join("src/lib.rs")).unwrap_or_else(|_| root.join("src/lib.rs"));

    let prepare_params = CallHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&extra_path).unwrap(),
            },
            position: Position::new(0, 8), // helper in extra.rs
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let item = sqry_lsp::handlers::call_hierarchy::prepare(&session, &prepare_params)?
        .and_then(|mut items| items.pop())
        .expect("extra::helper item");

    let params = CallHierarchyIncomingCallsParams {
        item,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: Default::default(),
    };
    let response = sqry_lsp::handlers::call_hierarchy::incoming(&session, &params)?;
    let caller = response
        .items
        .iter()
        .find(|entry| entry.from.name == "use_extra_helper")
        .expect("cross-file caller");
    match parse_item_data(&caller.from) {
        TestCallHierarchyData::Saved { file_path, .. } => assert_eq!(file_path, lib_path),
        _ => bail!("expected saved caller metadata"),
    }
    assert!(!caller.from_ranges.is_empty());
    Ok(())
}

#[test]
fn incoming_calls_empty_returns_message_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let path = root.join("src/lib.rs");

    let prepare_params = CallHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&path).unwrap(),
            },
            position: Position::new(89, 5), // lonely_function
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let item = sqry_lsp::handlers::call_hierarchy::prepare(&session, &prepare_params)?
        .expect("call hierarchy item")
        .into_iter()
        .next()
        .unwrap();

    let params = CallHierarchyIncomingCallsParams {
        item,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: Default::default(),
    };

    let response = sqry_lsp::handlers::call_hierarchy::incoming(&session, &params)?;
    assert!(response.items.is_empty());
    assert!(!response.is_truncated);
    Ok(())
}

#[test]
fn incoming_calls_unsaved_returns_error_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let path = root.join("src/lib.rs");
    let mut text = fs::read_to_string(&path)?;
    text.push_str("\nfn temp_unsaved() { format_state(7); }\n");
    let new_line = (text.lines().count() - 1) as u32;

    session
        .documents()
        .open(
            &path,
            Some("rust".into()),
            99,
            &text,
            &common::test_limits(),
        )
        .expect("open document for test");

    let params = CallHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&path).unwrap(),
            },
            position: Position::new(new_line, 5),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };

    let item = sqry_lsp::handlers::call_hierarchy::prepare(&session, &params)?
        .expect("call hierarchy item")
        .into_iter()
        .next()
        .unwrap();

    if let TestCallHierarchyData::Unsaved { message } = parse_item_data(&item) {
        assert!(message.contains("Save file"));
    } else {
        bail!("expected unsaved data");
    }

    let params = CallHierarchyIncomingCallsParams {
        item,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: Default::default(),
    };

    let err = sqry_lsp::handlers::call_hierarchy::incoming(&session, &params)
        .expect_err("expected unsaved error");
    match err {
        sqry_lsp::handlers::call_hierarchy::CallHierarchyError::UnsavedBuffer { .. } => {}
        other => bail!("unexpected error: {other:?}"),
    }
    Ok(())
}

#[test]
fn call_hierarchy_incoming_without_prepare_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let path = root.join("src/lib.rs");

    let bogus_item = CallHierarchyItem {
        name: "bogus".into(),
        kind: tower_lsp::lsp_types::SymbolKind::FUNCTION,
        tags: None,
        detail: None,
        uri: Url::from_file_path(&path).unwrap(),
        range: Default::default(),
        selection_range: Default::default(),
        data: None,
    };

    let params = CallHierarchyIncomingCallsParams {
        item: bogus_item,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: Default::default(),
    };

    let err = sqry_lsp::handlers::call_hierarchy::incoming(&session, &params)
        .expect_err("expected invalid data error");
    match err {
        sqry_lsp::handlers::call_hierarchy::CallHierarchyError::InvalidData(_) => {}
        other => bail!("unexpected error: {other:?}"),
    }
    Ok(())
}

#[test]
fn outgoing_calls_return_callees_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let path = root.join("src/lib.rs");

    let prepare_params = CallHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&path).unwrap(),
            },
            position: Position::new(58, 5), // format_state
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let item = sqry_lsp::handlers::call_hierarchy::prepare(&session, &prepare_params)?
        .expect("call hierarchy item")
        .into_iter()
        .next()
        .unwrap();

    let params = CallHierarchyOutgoingCallsParams {
        item,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: Default::default(),
    };

    let response = sqry_lsp::handlers::call_hierarchy::outgoing(&session, &params)?;
    assert!(!response.items.is_empty());
    Ok(())
}

#[test]
fn incoming_calls_respect_max_results_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let path = root.join("src/lib.rs");

    session.apply_client_settings(&json!({
        "sqry": { "callHierarchy": { "maxResults": 1 } }
    }))?;

    let prepare_params = CallHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&path).unwrap(),
            },
            position: Position::new(40, 5),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let item = sqry_lsp::handlers::call_hierarchy::prepare(&session, &prepare_params)?
        .expect("call hierarchy item")
        .into_iter()
        .next()
        .unwrap();

    let params = CallHierarchyIncomingCallsParams {
        item,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: Default::default(),
    };
    let response = sqry_lsp::handlers::call_hierarchy::incoming(&session, &params)?;
    assert_eq!(response.items.len(), 1);
    assert!(response.is_truncated || response.total <= 1);
    Ok(())
}

#[test]
fn multiple_calls_on_single_line_have_distinct_ranges_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let path = root.join("src/lib.rs");

    let prepare_params = CallHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&path).unwrap(),
            },
            position: Position::new(58, 5),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };

    let item = sqry_lsp::handlers::call_hierarchy::prepare(&session, &prepare_params)?
        .expect("call hierarchy item")
        .into_iter()
        .next()
        .unwrap();

    let params = CallHierarchyIncomingCallsParams {
        item,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: Default::default(),
    };

    let response = sqry_lsp::handlers::call_hierarchy::incoming(&session, &params)?;
    let multi = response
        .items
        .into_iter()
        .find(|entry| match parse_item_data(&entry.from) {
            TestCallHierarchyData::Saved { qualified_name, .. } => {
                qualified_name.ends_with("multi_call_line")
            }
            _ => false,
        })
        .expect("multi_call_line entry");
    assert!(!multi.from_ranges.is_empty());
    let mut unique = std::collections::BTreeSet::new();
    for range in &multi.from_ranges {
        unique.insert(range.start.character);
    }
    assert_eq!(unique.len(), multi.from_ranges.len());
    Ok(())
}

#[test]
fn recursive_function_reports_self_call_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let path = root.join("src/lib.rs");

    let prepare_params = CallHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&path).unwrap(),
            },
            position: Position::new(63, 5), // recursive
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };

    let item = sqry_lsp::handlers::call_hierarchy::prepare(&session, &prepare_params)?
        .expect("call hierarchy item")
        .into_iter()
        .next()
        .unwrap();

    let params = CallHierarchyOutgoingCallsParams {
        item,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: Default::default(),
    };

    let response = sqry_lsp::handlers::call_hierarchy::outgoing(&session, &params)?;
    assert!(!response.items.is_empty());
    let first = &response.items[0];
    assert_eq!(first.to.name, "recursive");
    assert!(!first.from_ranges.is_empty());
    Ok(())
}

#[test]
fn emoji_call_site_has_precise_range_new() -> Result<()> {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    let session = new_session(&root);
    let path = root.join("src/lib.rs");

    let prepare_params = CallHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(&path).unwrap(),
            },
            position: Position::new(77, 5), // rocket_launcher
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };

    let item = sqry_lsp::handlers::call_hierarchy::prepare(&session, &prepare_params)?
        .expect("call hierarchy item")
        .into_iter()
        .next()
        .unwrap();

    let params = CallHierarchyIncomingCallsParams {
        item,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: Default::default(),
    };

    let response = sqry_lsp::handlers::call_hierarchy::incoming(&session, &params)?;
    assert!(!response.items.is_empty());
    let first = &response.items[0];
    assert!(!first.from_ranges.is_empty());
    let range = first.from_ranges[0];
    assert!(
        range.start.character > 0,
        "expected non-zero column for emoji call site"
    );
    Ok(())
}
