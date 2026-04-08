use sqry_core::plugin::LanguagePlugin;
use sqry_lang_elixir::ElixirPlugin;

#[test]
fn debug_ast_structure() {
    let plugin = ElixirPlugin::default();
    let content = br"
defmodule Demo do
  def my_func, do: :ok
end
";

    let tree = plugin.parse_ast(content).expect("should parse");
    let root = tree.root_node();

    eprintln!("Root node kind: {}", root.kind());

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        eprintln!("Child kind: {} (named: {})", child.kind(), child.is_named());
        if child.kind() == "call" {
            eprintln!("  Found call node");
            if let Some(identifier) = child.child_by_field_name("identifier") {
                let text = identifier.utf8_text(content).unwrap_or("???");
                eprintln!("  identifier: {text}");
            }
            if let Some(target) = child.child_by_field_name("target") {
                let text = target.utf8_text(content).unwrap_or("???");
                eprintln!("  target: {text}");
            }
        }
    }
}

#[test]
#[ignore = "AST exploration for type specs"]
fn explore_type_spec_ast() {
    use tree_sitter::Node;

    fn print_ast(node: Node, content: &[u8], depth: usize) {
        let indent = "  ".repeat(depth);
        let kind = node.kind();
        let text = if node.named_child_count() == 0 {
            node.utf8_text(content).unwrap_or("")
        } else {
            ""
        };
        println!("{indent}{kind} '{text}'");
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            print_ast(child, content, depth + 1);
        }
    }

    let source = r"
defmodule User do
  @type t :: %{name: String.t(), age: integer()}

  @spec create(String.t(), integer()) :: t()
  def create(name, age) do
    %{name: name, age: age}
  end
end
";

    let plugin = crate::ElixirPlugin::default();
    let tree = plugin.parse_ast(source.as_bytes()).expect("parse failed");
    println!("\n=== ELIXIR TYPE SPEC AST ===\n");
    print_ast(tree.root_node(), source.as_bytes(), 0);
}
