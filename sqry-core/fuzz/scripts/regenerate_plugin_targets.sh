#!/bin/bash
# Regenerate all plugin fuzz targets with correct syntax

cd "$(dirname "$0")/../fuzz_targets"

# Plugin configurations: name, crate, struct, extension
declare -A PLUGINS=(
    ["c_plugin"]="sqry_lang_c:CPlugin:c"
    ["cpp_plugin"]="sqry_lang_cpp:CppPlugin:cpp"
    ["csharp_plugin"]="sqry_lang_csharp:CSharpPlugin:cs"
    ["css_plugin"]="sqry_lang_css:CssPlugin:css"
    ["dart_plugin"]="sqry_lang_dart:DartPlugin:dart"
    ["elixir_plugin"]="sqry_lang_elixir:ElixirPlugin:ex"
    ["go_plugin"]="sqry_lang_go:GoPlugin:go"
    ["groovy_plugin"]="sqry_lang_groovy:GroovyPlugin:groovy"
    ["haskell_plugin"]="sqry_lang_haskell:HaskellPlugin:hs"
    ["html_plugin"]="sqry_lang_html:HtmlPlugin:html"
    ["java_plugin"]="sqry_lang_java:JavaPlugin:java"
    ["javascript_plugin"]="sqry_lang_javascript:JavaScriptPlugin:js"
    ["kotlin_plugin"]="sqry_lang_kotlin:KotlinPlugin:kt"
    ["lua_plugin"]="sqry_lang_lua:LuaPlugin:lua"
    ["oracle_plsql_plugin"]="sqry_lang_oracle_plsql:OraclePlsqlPlugin:sql"
    ["perl_plugin"]="sqry_lang_perl:PerlPlugin:pl"
    ["php_plugin"]="sqry_lang_php:PhpPlugin:php"
    ["puppet_plugin"]="sqry_lang_puppet:PuppetPlugin:pp"
    ["python_plugin"]="sqry_lang_python:PythonPlugin:py"
    ["r_plugin"]="sqry_lang_r:RPlugin:r"
    ["ruby_plugin"]="sqry_lang_ruby:RubyPlugin:rb"
    ["rust_plugin"]="sqry_lang_rust:RustPlugin:rs"
    ["salesforce_apex_plugin"]="sqry_lang_salesforce_apex:SalesforceApexPlugin:cls"
    ["sap_abap_plugin"]="sqry_lang_sap_abap:SapAbapPlugin:abap"
    ["scala_plugin"]="sqry_lang_scala:ScalaPlugin:scala"
    ["servicenow_xanadu_plugin"]="sqry_lang_servicenow_xanadu:ServicenowXanaduPlugin:js"
    ["shell_plugin"]="sqry_lang_shell:ShellPlugin:sh"
    ["sql_plugin"]="sqry_lang_sql:SqlPlugin:sql"
    ["svelte_plugin"]="sqry_lang_svelte:SveltePlugin:svelte"
    ["swift_plugin"]="sqry_lang_swift:SwiftPlugin:swift"
    ["terraform_plugin"]="sqry_lang_terraform:TerraformPlugin:tf"
    ["typescript_plugin"]="sqry_lang_typescript:TypeScriptPlugin:ts"
    ["vue_plugin"]="sqry_lang_vue:VuePlugin:vue"
    ["zig_plugin"]="sqry_lang_zig:ZigPlugin:zig"
)

for target in "${!PLUGINS[@]}"; do
    IFS=':' read -r crate struct ext <<< "${PLUGINS[$target]}"

    cat > "${target}.rs" << EOF
#![no_main]
use libfuzzer_sys::fuzz_target;
use sqry_core::plugin::LanguagePlugin;
use ${crate}::${struct};
use std::path::Path;

fuzz_target!(|data: &[u8]| {
    let plugin = ${struct}::default();
    let dummy_path = Path::new("fuzz.${ext}");

    // Fuzz AST parsing
    if let Ok(tree) = plugin.parse_ast(data) {
        // If parsing succeeds, fuzz symbol extraction
        let _ = plugin.extract_symbols_from_tree(&tree, data, dummy_path);
    }
});
EOF
    echo "Generated: ${target}.rs"
done

echo "Done. Generated ${#PLUGINS[@]} plugin fuzz targets."
