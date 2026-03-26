use sqry_plugin_registry::create_plugin_manager;

#[test]
fn test_registry_parity() {
    let pm = create_plugin_manager();
    let plugins = pm.plugins();

    // Explicit roster of expected language IDs and extensions in registration order.
    // Note: These must match LanguageMetadata::id and LanguagePlugin::extensions.
    let expected = vec![
        // General
        ("c", &["c", "h"][..]),
        ("cpp", &["cpp", "cc", "cxx", "hpp", "hh", "hxx"][..]),
        ("csharp", &["cs", "csx"][..]),
        ("css", &["css", "scss", "sass", "less"][..]),
        ("dart", &["dart"][..]),
        ("elixir", &["ex", "exs"][..]),
        ("go", &["go"][..]),
        ("groovy", &["groovy", "gradle", "gvy", "gy", "gsh"][..]),
        ("haskell", &["hs", "lhs", "hs-boot"][..]),
        ("html", &["html", "htm", "xhtml"][..]),
        ("java", &["java"][..]),
        ("javascript", &["js", "jsx", "mjs"][..]),
        ("kotlin", &["kt", "kts"][..]),
        ("lua", &["lua", "rockspec"][..]),
        ("perl", &["pl", "pm", "t"][..]),
        ("php", &["php"][..]),
        ("python", &["py", "pyi"][..]),
        ("r", &["r", "rmd", "q"][..]),
        ("ruby", &["rb", "rake", "gemspec"][..]),
        ("rust", &["rs"][..]),
        ("scala", &["scala", "sc"][..]),
        (
            "shell",
            &["sh", "bash", "bashrc", "bash_profile", "profile", "env"][..],
        ),
        ("sql", &["sql"][..]),
        ("svelte", &["svelte"][..]),
        ("swift", &["swift"][..]),
        ("typescript", &["ts", "tsx"][..]),
        ("vue", &["vue"][..]),
        ("zig", &["zig", "zon"][..]),
        // Domain-specific
        (
            "plsql",
            &["pks", "pkb", "pls", "plb", "prc", "fnc", "trg"][..],
        ),
        ("apex", &["cls", "trigger"][..]),
        ("abap", &["abap"][..]),
        ("servicenow-xanadu-js", &["snjs"][..]),
        // IaC
        ("terraform", &["tf", "tfvars", "hcl"][..]),
        ("puppet", &["pp"][..]),
        ("pulumi", &["pulumi.yaml", "pulumi.yml", "pulumi.json"][..]),
        // Config
        ("json", &["json"][..]),
    ];

    let found: Vec<(&str, Vec<&str>)> = plugins
        .iter()
        .map(|plugin| (plugin.metadata().id, plugin.extensions().to_vec()))
        .collect();
    let expected_roster: Vec<(&str, Vec<&str>)> = expected
        .iter()
        .map(|(id, exts)| (*id, exts.to_vec()))
        .collect();

    assert_eq!(
        found, expected_roster,
        "Registry roster mismatch (expected exact IDs + extensions in deterministic order)"
    );

    for (expected_id, exts) in &expected {
        let plugin = pm
            .plugin_by_id(expected_id)
            .unwrap_or_else(|| panic!("Missing plugin ID from registry: {expected_id}"));
        assert_eq!(
            plugin.metadata().id,
            *expected_id,
            "Plugin lookup by ID returned unexpected metadata for {expected_id}"
        );

        for &ext in *exts {
            let plugin = pm
                .plugin_for_extension(ext)
                .unwrap_or_else(|| panic!("Missing extension mapping for {expected_id}: {ext}"));
            assert_eq!(
                plugin.metadata().id,
                *expected_id,
                "Extension mapping mismatch for {expected_id}: {ext}"
            );
        }
    }
}
