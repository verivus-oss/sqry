/**
 * AST dumping utility to understand tree-sitter-sfapex structure
 */
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_salesforce_apex::SalesforceApexPlugin;
use tree_sitter::TreeCursor;

fn print_tree(cursor: &mut TreeCursor, content: &[u8], depth: usize) {
    let node = cursor.node();
    let kind = node.kind();
    let start = node.start_byte();
    let end = node.end_byte();

    let indent = "  ".repeat(depth);
    let text = if end - start < 50 {
        String::from_utf8_lossy(&content[start..end]).replace('\n', "\\n")
    } else {
        format!(
            "{}...",
            String::from_utf8_lossy(&content[start..(start + 47)]).replace('\n', "\\n")
        )
    };

    println!("{indent}{kind} [{start}-{end}]: \"{text}\"");

    if cursor.goto_first_child() {
        loop {
            print_tree(cursor, content, depth + 1);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

#[test]
fn dump_simple_soql() {
    let plugin = SalesforceApexPlugin::new();

    let code = b"public class Test {
    public static List<Account> getAccounts() {
        return [SELECT Id, Name FROM Account];
    }
}";

    let tree = plugin.parse_ast(code).expect("Should parse");
    let mut cursor = tree.walk();

    println!("\n=== Simple SOQL Query AST ===");
    print_tree(&mut cursor, code, 0);
}

#[test]
fn dump_simple_dml() {
    let plugin = SalesforceApexPlugin::new();

    let code = b"public class Test {
    public static void createAccount() {
        Account acc = new Account(Name='Test');
        insert acc;
    }
}";

    let tree = plugin.parse_ast(code).expect("Should parse");
    let mut cursor = tree.walk();

    println!("\n=== Simple DML Operation AST ===");
    print_tree(&mut cursor, code, 0);
}

#[test]
fn dump_simple_annotation() {
    let plugin = SalesforceApexPlugin::new();

    let code = b"public class Test {
    @AuraEnabled
    public static List<Account> getAccounts() {
        return [SELECT Id FROM Account];
    }
}";

    let tree = plugin.parse_ast(code).expect("Should parse");
    let mut cursor = tree.walk();

    println!("\n=== Simple Annotation AST ===");
    print_tree(&mut cursor, code, 0);
}

#[test]
fn dump_simple_trigger() {
    let plugin = SalesforceApexPlugin::new();

    let code = b"trigger AccountTrigger on Account (before insert, after update) {
    System.debug('Trigger fired');
}";

    let tree = plugin.parse_ast(code).expect("Should parse");
    let mut cursor = tree.walk();

    println!("\n=== Simple Trigger AST ===");
    print_tree(&mut cursor, code, 0);
}
