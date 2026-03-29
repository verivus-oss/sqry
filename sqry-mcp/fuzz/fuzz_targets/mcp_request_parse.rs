#![no_main]
use libfuzzer_sys::fuzz_target;
use rmcp::model::ClientJsonRpcMessage;

fuzz_target!(|data: &[u8]| {
    // Fuzz JSON-RPC request parsing
    // We don't care if parsing succeeds or fails, only that it doesn't panic
    if let Ok(json_str) = std::str::from_utf8(data) {
        let _: Result<ClientJsonRpcMessage, _> = serde_json::from_str(json_str);
    }

    // Also try to parse directly from bytes
    let _: Result<ClientJsonRpcMessage, _> = serde_json::from_slice(data);
});
