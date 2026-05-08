use sqry_plugin_registry::{
    HighCostMode, PluginSelectionConfig, create_plugin_manager, create_plugin_manager_all,
};

fn expected_roster() -> Vec<(&'static str, &'static [&'static str])> {
    let mut roster: Vec<(&'static str, &'static [&'static str])> = vec![
        ("c", &["c", "h"]),
        ("cpp", &["cpp", "cc", "cxx", "hpp", "hh", "hxx"]),
        ("csharp", &["cs", "csx"]),
        ("css", &["css", "scss", "sass", "less"]),
        ("dart", &["dart"]),
        ("elixir", &["ex", "exs"]),
        ("go", &["go"]),
        ("groovy", &["groovy", "gradle", "gvy", "gy", "gsh"]),
        ("haskell", &["hs", "lhs", "hs-boot"]),
        ("html", &["html", "htm", "xhtml"]),
        ("java", &["java"]),
        ("javascript", &["js", "jsx", "mjs"]),
        ("kotlin", &["kt", "kts"]),
        ("lua", &["lua", "rockspec"]),
        ("perl", &["pl", "pm", "t"]),
        ("php", &["php"]),
        ("python", &["py", "pyi"]),
        ("r", &["r", "rmd", "q"]),
        ("ruby", &["rb", "rake", "gemspec"]),
        ("rust", &["rs"]),
        ("scala", &["scala", "sc"]),
        (
            "shell",
            &["sh", "bash", "bashrc", "bash_profile", "profile", "env"],
        ),
        ("sql", &["sql"]),
        ("svelte", &["svelte"]),
        ("swift", &["swift"]),
        ("typescript", &["ts", "tsx"]),
        ("vue", &["vue"]),
        ("zig", &["zig", "zon"]),
        ("plsql", &["pks", "pkb", "pls", "plb", "prc", "fnc", "trg"]),
    ];

    #[cfg(feature = "plugin-apex")]
    roster.push(("apex", &["cls", "trigger"]));
    #[cfg(feature = "plugin-abap")]
    roster.push(("abap", &["abap"]));
    #[cfg(feature = "plugin-servicenow-xanadu")]
    roster.push(("servicenow-xanadu-js", &["snjs"]));
    #[cfg(feature = "plugin-servicenow-xml")]
    roster.push(("servicenow-xml", &["xml"]));
    #[cfg(feature = "plugin-terraform")]
    roster.push(("terraform", &["tf", "tfvars", "hcl"]));
    #[cfg(feature = "plugin-puppet")]
    roster.push(("puppet", &["pp"]));
    #[cfg(feature = "plugin-pulumi")]
    roster.push(("pulumi", &["pulumi.yaml", "pulumi.yml", "pulumi.json"]));

    roster.push(("json", &["json"]));
    roster
}

#[test]
fn test_registry_parity_for_full_builtin_roster() {
    let plugin_manager = create_plugin_manager_all();
    let expected_roster_values: Vec<(&str, Vec<&str>)> = expected_roster()
        .iter()
        .map(|(plugin_id, extensions)| (*plugin_id, extensions.to_vec()))
        .collect();
    let actual_roster: Vec<(&str, Vec<&str>)> = plugin_manager
        .plugins()
        .iter()
        .map(|plugin| (plugin.metadata().id, plugin.extensions().to_vec()))
        .collect();

    assert_eq!(
        actual_roster, expected_roster_values,
        "full registry roster must keep ids, extensions, and order stable",
    );

    for (plugin_id, extensions) in expected_roster() {
        let plugin = plugin_manager
            .plugin_by_id(plugin_id)
            .unwrap_or_else(|| panic!("missing plugin id {plugin_id}"));
        assert_eq!(plugin.metadata().id, plugin_id);

        for extension in extensions {
            let extension_plugin = plugin_manager
                .plugin_for_extension(extension)
                .unwrap_or_else(|| {
                    panic!("missing extension mapping for {plugin_id}: {extension}")
                });
            assert_eq!(extension_plugin.metadata().id, plugin_id);
        }
    }
}

#[test]
fn test_default_fast_path_excludes_high_cost_plugins() {
    let plugin_manager = create_plugin_manager();
    let ids: Vec<&str> = plugin_manager
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id)
        .collect();

    assert_eq!(
        ids.len(),
        expected_roster()
            .iter()
            .filter(|(plugin_id, _)| *plugin_id != "json")
            .filter(|(plugin_id, _)| !plugin_id.starts_with("servicenow-xml"))
            .filter(|(plugin_id, _)| !matches!(
                *plugin_id,
                "apex" | "abap" | "servicenow-xanadu-js" | "terraform" | "puppet" | "pulumi"
            ))
            .count()
    );
    assert!(!ids.contains(&"json"));
    #[cfg(feature = "plugin-servicenow-xml")]
    assert!(!ids.contains(&"servicenow-xml"));
}

#[test]
fn test_explicit_high_cost_enable_restores_json_without_source_edits() {
    let config = PluginSelectionConfig {
        high_cost_mode: HighCostMode::FastPathDefault,
        enable_plugins: std::collections::BTreeSet::from([String::from("json")]),
        disable_plugins: std::collections::BTreeSet::new(),
    };
    let plugin_manager = sqry_plugin_registry::create_plugin_manager_with_config(&config).unwrap();

    assert!(plugin_manager.plugin_by_id("json").is_some());
    assert!(plugin_manager.plugin_for_extension("json").is_some());
}

#[test]
fn test_exclude_all_high_cost_matches_fast_path_expectation() {
    let plugin_manager =
        sqry_plugin_registry::create_plugin_manager_with_config(&PluginSelectionConfig {
            high_cost_mode: HighCostMode::ExcludeAll,
            ..PluginSelectionConfig::default()
        })
        .expect("exclude-all config should resolve");

    assert!(plugin_manager.plugin_by_id("json").is_none());
    #[cfg(feature = "plugin-servicenow-xml")]
    assert!(plugin_manager.plugin_by_id("servicenow-xml").is_none());
}

#[test]
fn test_explicit_disable_beats_enable_in_include_all_mode() {
    let plugin_manager =
        sqry_plugin_registry::create_plugin_manager_with_config(&PluginSelectionConfig {
            high_cost_mode: HighCostMode::IncludeAll,
            enable_plugins: std::collections::BTreeSet::from([String::from("json")]),
            disable_plugins: std::collections::BTreeSet::from([String::from("json")]),
        })
        .expect("include-all config should resolve");

    assert!(plugin_manager.plugin_by_id("json").is_none());
}
