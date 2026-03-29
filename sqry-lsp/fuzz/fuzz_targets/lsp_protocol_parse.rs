#![no_main]
use libfuzzer_sys::fuzz_target;
use sqry_lsp::protocol::{
    RelationKind, SqryIndexStatusParams, SqryRelationParams, SqrySearchParams,
};

fuzz_target!(|data: &[u8]| {
    // Fuzz LSP protocol parameter deserialization
    // We don't care if parsing succeeds or fails, only that it doesn't panic

    if let Ok(json_str) = std::str::from_utf8(data) {
        // Try parsing as each protocol type
        let _: Result<SqrySearchParams, _> = serde_json::from_str(json_str);
        let _: Result<SqryRelationParams, _> = serde_json::from_str(json_str);
        let _: Result<SqryIndexStatusParams, _> = serde_json::from_str(json_str);
        let _: Result<RelationKind, _> = serde_json::from_str(json_str);
    }

    // Also try direct byte deserialization
    let _: Result<SqrySearchParams, _> = serde_json::from_slice(data);
    let _: Result<SqryRelationParams, _> = serde_json::from_slice(data);
    let _: Result<SqryIndexStatusParams, _> = serde_json::from_slice(data);
});
