use sqry_core::plugin::LanguagePlugin;
use sqry_lang_elixir::ElixirPlugin;

#[test]
fn debug_def_structure() {
    let plugin = ElixirPlugin::default();
    let content = br"
defmodule Demo do
  def my_func, do: :ok
end
";

    let tree = plugin.parse_ast(content).expect("should parse");
    let root = tree.root_node();

    #[allow(clippy::items_after_statements)] // Const defined near usage for clarity
    fn print_tree(node: tree_sitter::Node, content: &[u8], depth: usize) {
        if !node.is_named() {
            return;
        }

        let indent = "  ".repeat(depth);
        let text = node.utf8_text(content).unwrap_or("???");
        let text_preview = if text.len() > 30 { &text[..30] } else { text };

        eprintln!("{}kind={}, text='{}'", indent, node.kind(), text_preview);

        if node.kind() == "call"
            && let Some(target) = node.child_by_field_name("target")
        {
            eprintln!(
                "{}  [field:target] {}",
                indent,
                target.utf8_text(content).unwrap_or("???")
            );
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.is_named() {
                print_tree(child, content, depth + 1);
            }
        }
    }

    print_tree(root, content, 0);
}
