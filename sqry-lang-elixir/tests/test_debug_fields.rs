use sqry_core::plugin::LanguagePlugin;
use sqry_lang_elixir::ElixirPlugin;

#[test]
fn debug_node_fields() {
    let plugin = ElixirPlugin::default();
    let content = br#"
defmodule Demo do
  def my_func, do: :ok
end
"#;

    let tree = plugin.parse_ast(content).expect("should parse");
    let root = tree.root_node();

    eprintln!("\n=== Root node ===");
    eprintln!("Kind: {}", root.kind());

    let mut cursor = root.walk();
    for (i, child) in root.children(&mut cursor).enumerate() {
        if !child.is_named() {
            continue;
        }

        eprintln!("\n=== Child {} ===", i);
        eprintln!("Kind: {}", child.kind());

        // Try all possible field names
        for field_name in ["identifier", "target", "arguments", "do_block"] {
            if let Some(field_node) = child.child_by_field_name(field_name) {
                let text = field_node.utf8_text(content).unwrap_or("???");
                eprintln!("  {}: {} (kind: {})", field_name, text, field_node.kind());
            }
        }

        // Also show direct children
        eprintln!("  Direct children:");
        let mut child_cursor = child.walk();
        for (j, grandchild) in child.children(&mut child_cursor).enumerate() {
            if grandchild.is_named() {
                let text = grandchild.utf8_text(content).unwrap_or("???");
                eprintln!(
                    "    [{}] {} (kind: {})",
                    j,
                    if text.len() > 30 { &text[..30] } else { text },
                    grandchild.kind()
                );
            }
        }
    }
}
