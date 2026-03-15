// Baseline memory profiling for Ruby parsing (no relation extraction)
//
// Run with:
//   cargo build --release --example memory_baseline --package sqry-lang-ruby
//   valgrind --tool=massif --massif-out-file=massif_baseline.out \
//     target/release/examples/memory_baseline
//   ms_print massif_baseline.out | grep "peak"

use sqry_core::plugin::LanguagePlugin;
use sqry_lang_ruby::RubyPlugin;
use std::fmt::Write as _;

fn generate_large_ruby_file() -> String {
    let mut code = String::new();

    code.push_str("# Large Ruby file for memory profiling\n\n");

    // Generate ~1000 lines of Ruby with realistic patterns
    for i in 0..200 {
        write!(
            code,
            r#"
class Handler{i}
  def initialize
    @state = {{}}
    @logger = Logger.new
  end

  def process(data)
    validate(data)
    transform(data)
    save(data)
  end

  def validate(data)
    data&.valid? && data.respond_to?(:save)
  end

  def transform(data)
    data.map {{ |item| item.upcase }}
  end

  def save(data)
    Database.send(:insert, data)
    Cache.public_send(:clear)
  end

  private

  def internal_method
    "internal"
  end
end
"#,
        )
        .expect("write ruby fixture");
    }

    code
}

fn main() {
    let plugin = RubyPlugin::default();
    let ruby_code = generate_large_ruby_file();

    println!("Generated {} bytes of Ruby code", ruby_code.len());
    println!("Running baseline parsing (no relation extraction)...");

    // Parse AST only - no relation extraction
    let _tree = plugin
        .parse_ast(ruby_code.as_bytes())
        .expect("Failed to parse Ruby code");

    println!("Baseline parsing complete");
}
