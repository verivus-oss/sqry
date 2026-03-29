#![no_main]
use libfuzzer_sys::fuzz_target;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_sap_abap::SapAbapPlugin;
use std::path::Path;

fuzz_target!(|data: &[u8]| {
    let plugin = SapAbapPlugin::default();
    let dummy_path = Path::new("fuzz.abap");

    // Fuzz AST parsing
    if let Ok(tree) = plugin.parse_ast(data) {
        // If parsing succeeds, fuzz symbol extraction
        let _ = plugin.extract_symbols_from_tree(&tree, data, dummy_path);
    }
});
