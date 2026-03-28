#![no_main]
use libfuzzer_sys::fuzz_target;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_cpp::CppPlugin;
use std::path::Path;

fuzz_target!(|data: &[u8]| {
    let plugin = CppPlugin::default();
    let dummy_path = Path::new("fuzz.cpp");

    // Fuzz AST parsing
    if let Ok(tree) = plugin.parse_ast(data) {
        // If parsing succeeds, fuzz symbol extraction
        let _ = plugin.extract_symbols_from_tree(&tree, data, dummy_path);
    }
});
