//! Explore Apex AST structure for interfaces and enums

#[test]
fn explore_interface_enum_ast() {
    let code = r#"
public interface Payable {
    Decimal calculatePayment();
}

public enum Status {
    PENDING,
    APPROVED
}

public class Invoice extends BaseInvoice implements Payable {
    public Decimal calculatePayment() {
        return 100.0;
    }
}
"#;

    let mut parser = tree_sitter::Parser::new();
    let language: tree_sitter::Language = tree_sitter_sfapex::apex::LANGUAGE.into();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(code.as_bytes(), None).unwrap();

    fn print_node(node: tree_sitter::Node, content: &[u8], indent: usize) {
        let text = node.utf8_text(content).unwrap_or("");
        let preview = if text.len() > 60 {
            format!("{}...", &text[..60].replace('\n', " "))
        } else {
            text.replace('\n', " ")
        };
        println!(
            "{:indent$}{} [{}..{}]: {:?}",
            "",
            node.kind(),
            node.start_byte(),
            node.end_byte(),
            preview,
            indent = indent * 2
        );

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            print_node(child, content, indent + 1);
        }
    }

    println!("\n=== APEX AST STRUCTURE ===");
    print_node(tree.root_node(), code.as_bytes(), 0);
}
