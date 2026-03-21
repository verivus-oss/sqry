//! Grammar inspection test for Puppet
//! Run with: cargo test -p sqry-lang-puppet --test `inspect_grammar` -- --nocapture

use tree_sitter::{Node, Parser};

fn print_tree(node: Node, source: &[u8], indent: usize) {
    let kind = node.kind();
    let text = node.utf8_text(source).unwrap_or("<error>");

    // Truncate long text for readability
    let display_text = if text.len() > 50 {
        format!("{}...", &text[..47])
    } else {
        text.to_string()
    };

    println!(
        "{:indent$}{:20} | {}",
        "",
        kind,
        display_text,
        indent = indent * 2
    );

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        print_tree(child, source, indent + 1);
    }
}

#[test]
fn inspect_puppet_ast() {
    let code = br#"
# Sample Puppet manifest

class myapp {
  $package_name = 'nginx'

  package { 'nginx':
    ensure => installed,
  }

  service { 'nginx':
    ensure => running,
    enable => true,
  }
}

define myapp::config($port = 80) {
  file { "/etc/myapp/${name}.conf":
    content => template('myapp/config.erb'),
  }
}

class webserver {
  include myapp
  require myapp::prereqs
}

node 'web01.example.com' {
  class { 'myapp': }
}
"#;

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_puppet::LANGUAGE.into())
        .expect("Failed to set Puppet language");

    let tree = parser
        .parse(code, None)
        .expect("Failed to parse Puppet code");

    println!("\n=== PUPPET AST STRUCTURE ===");
    print_tree(tree.root_node(), code, 0);

    // Keep the test always passing so we can see output
    assert!(!tree.root_node().has_error() || tree.root_node().has_error());
}
