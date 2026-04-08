use sqry_core::plugin::LanguagePlugin;
use sqry_lang_elixir::ElixirPlugin;
use tree_sitter::Node;

#[test]
fn debug_arguments_structure() {
    let plugin = ElixirPlugin::default();
    let content = br"
defmodule Demo do
  def my_func, do: :ok
end
";

    let tree = plugin.parse_ast(content).expect("should parse");
    let root = tree.root_node();

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if is_call_node(child) {
            debug_call_node(child, content);
        }
    }
}

fn is_call_node(node: Node<'_>) -> bool {
    node.is_named() && node.kind() == "call"
}

fn debug_call_node(node: Node<'_>, content: &[u8]) {
    let Some(target) = node.child_by_field_name("target") else {
        return;
    };
    let target_text = node_text(target, content);
    eprintln!("\n=== Call to: {target_text} ===");

    if let Some(args) = node.child_by_field_name("arguments") {
        print_arguments(args, content);
    } else {
        eprintln!("NO arguments field!");
    }

    if let Some(do_block) = node.child_by_field_name("do_block") {
        print_do_block(do_block, content);
    }
}

fn print_arguments(args: Node<'_>, content: &[u8]) {
    eprintln!("Arguments node:");
    eprintln!("  Kind: {}", args.kind());
    let args_text = node_text(args, content);
    eprintln!("  Text: '{}'", truncate(args_text, 80));

    let mut args_cursor = args.walk();
    for (i, arg_child) in args.children(&mut args_cursor).enumerate() {
        let text = node_text(arg_child, content);
        eprintln!(
            "  Child [{}]: named={}, kind={}, text='{}'",
            i,
            arg_child.is_named(),
            arg_child.kind(),
            truncate(text, 40)
        );
    }
}

fn print_do_block(do_block: Node<'_>, content: &[u8]) {
    eprintln!("\ndo_block:");
    let mut do_cursor = do_block.walk();
    for (i, do_child) in do_block.children(&mut do_cursor).enumerate() {
        if is_call_node(do_child) {
            print_do_block_call(i, do_child, content);
        }
    }
}

fn print_do_block_call(index: usize, node: Node<'_>, content: &[u8]) {
    eprintln!("  Call [{index}]:");
    if let Some(def_target) = node.child_by_field_name("target") {
        let def_text = node_text(def_target, content);
        eprintln!("    target: {def_text}");
    }
    if let Some(def_args) = node.child_by_field_name("arguments") {
        eprintln!("    arguments kind: {}", def_args.kind());
        let mut def_args_cursor = def_args.walk();
        for (j, def_arg_child) in def_args.children(&mut def_args_cursor).enumerate() {
            if def_arg_child.is_named() {
                let text = node_text(def_arg_child, content);
                eprintln!(
                    "      arg[{}]: kind={}, text={}",
                    j,
                    def_arg_child.kind(),
                    truncate(text, 20)
                );
            }
        }
    }
}

fn node_text<'a>(node: Node<'_>, content: &'a [u8]) -> &'a str {
    node.utf8_text(content).unwrap_or("???")
}

fn truncate(text: &str, max_len: usize) -> &str {
    if text.len() > max_len {
        &text[..max_len]
    } else {
        text
    }
}
