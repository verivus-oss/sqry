# Contributors

Thank you to everyone who has contributed to sqry!

## Core Team

**Verivus Labs (Open Source Team)**
- Primary development and maintenance
- Architecture and design
- Quality assurance and testing

Contact: hello@sqry.dev

---

## How to Contribute

We welcome contributions from the community! sqry is an open-source project under the MIT license.

### Ways to Contribute

1. **Report Bugs**: Open an issue on [GitHub Issues](https://github.com/verivus-oss/sqry/issues)
2. **Suggest Features**: Start a discussion in [GitHub Discussions](https://github.com/verivus-oss/sqry/discussions)
3. **Submit Code**: Open a pull request with bug fixes or new features
4. **Improve Documentation**: Help us make our docs clearer and more comprehensive
5. **Add Language Support**: Implement a new language plugin (see [CONTRIBUTING.md](CONTRIBUTING.md#adding-a-language-plugin))
6. **Write Tests**: Expand our test coverage
7. **Benchmarking**: Test sqry on your codebase and share performance data

### Getting Started

1. **Read the Documentation**
   - [README.md](README.md) - Project overview
   - [CONTRIBUTING.md](CONTRIBUTING.md) - Contribution guidelines
   - [QUICKSTART.md](QUICKSTART.md) - Quick start guide

2. **Set Up Development Environment**
   ```bash
   # Clone the repository
   git clone https://github.com/verivus-oss/sqry.git
   cd sqry

   # Build the project
   cargo build

   # Run tests
   cargo test

   # Run benchmarks
   cd benchmarks
   cargo bench
   ```

3. **Find an Issue to Work On**
   - Look for issues labeled `good first issue` or `help wanted`
   - Comment on the issue to let others know you're working on it
   - Ask questions if anything is unclear

4. **Submit Your Contribution**
   - Fork the repository
   - Create a feature branch (`git checkout -b feature/my-feature`)
   - Make your changes with clear commit messages
   - Add tests for new functionality
   - Run `cargo fmt` and `cargo clippy`
   - Push to your fork and open a pull request

### Code Review Process

1. **Automated Checks**: CI/CD runs tests, clippy, and rustfmt
2. **Code Review**: Core team reviews your PR
3. **Feedback**: Address any requested changes
4. **Approval**: Once approved, your PR will be merged
5. **Release**: Your contribution will be included in the next release

---

## Community Guidelines

### Code of Conduct

sqry follows the [Rust Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct). We are committed to providing a welcoming and inclusive environment for all contributors.

Key principles:
- **Be respectful**: Treat everyone with respect and kindness
- **Be constructive**: Provide helpful, actionable feedback
- **Be collaborative**: Work together to solve problems
- **Be patient**: Everyone has different levels of experience

### Communication Channels

- **GitHub Issues**: Bug reports and feature requests
- **GitHub Discussions**: Questions, ideas, and general discussion
- **Pull Requests**: Code contributions and reviews

---

## Recognition

### Contributors by Area

#### Language Plugins

Help us expand language support! Current language plugins (35 total):

**Full relation support (28)**: C, C++, C#, CSS, Dart, Elixir, Go, Groovy, Haskell, HTML, Java, JavaScript, Kotlin, Lua, Perl, PHP, Python, R, Ruby, Rust, Scala, Shell, SQL, Svelte, Swift, TypeScript, Vue, Zig

**Symbol extraction + imports (7)**: Terraform, Puppet, Pulumi, Salesforce Apex, SAP ABAP, Oracle PL/SQL, ServiceNow Xanadu

Want to add a language? See [CONTRIBUTING.md](CONTRIBUTING.md#adding-a-language-plugin).

#### Documentation

- User guides (Quick Start, Examples, Performance, etc.)
- API documentation
- Plugin development guide
- Migration guides

#### Testing & Quality

- Unit tests (24,000+ passing tests)
- Integration tests
- E2E tests for CLI commands
- Benchmark suite

#### Infrastructure

- CI/CD pipeline (GitHub Actions)
- Release automation (release-plz)
- Performance regression detection
- Code quality tools (clippy, rustfmt)

---

## Attribution

sqry builds on excellent open-source projects:

### Core Dependencies

- **[tree-sitter](https://github.com/tree-sitter/tree-sitter)** - Fast, incremental parsing
- **[ripgrep](https://github.com/BurntSushi/ripgrep)** - Fast text search (used as fallback search library)
- **[clap](https://github.com/clap-rs/clap)** - Command-line argument parsing
- **[serde](https://github.com/serde-rs/serde)** - Serialization framework
- **[rayon](https://github.com/rayon-rs/rayon)** - Data parallelism
- **[tokio](https://github.com/tokio-rs/tokio)** - Async runtime

### Tree-Sitter Grammars

We use and appreciate the following tree-sitter grammar maintainers:

- [tree-sitter-rust](https://github.com/tree-sitter/tree-sitter-rust)
- [tree-sitter-python](https://github.com/tree-sitter/tree-sitter-python)
- [tree-sitter-typescript](https://github.com/tree-sitter/tree-sitter-typescript)
- [tree-sitter-javascript](https://github.com/tree-sitter/tree-sitter-javascript)
- [tree-sitter-go](https://github.com/tree-sitter/tree-sitter-go)
- [tree-sitter-java](https://github.com/tree-sitter/tree-sitter-java)
- And many more!

---

## Recognition

Contributors who make significant contributions will be recognized in:

1. **Release Notes**: Mentioned in the changelog for their contributions
2. **This File**: Added to a contributors list (if desired)
3. **GitHub**: Contributor badge on the repository

### Notable Contributions

We especially appreciate contributions that:

- Add support for a new programming language
- Significantly improve performance
- Fix critical bugs
- Improve documentation clarity
- Add comprehensive test coverage
- Help with release management

---

## Contributor Statistics

As of v4.8.2:

- **Total Commits**: 2,300+
- **Total Tests**: 24,000+
- **Languages Supported**: 35
- **Lines of Code**: 476,000+

---

## Future Contributors

We're looking for help with:

### High Priority

1. **Language Plugins**
   - Add new language plugins (see [CONTRIBUTING.md](CONTRIBUTING.md#adding-a-language-plugin))
   - Improve relation extraction for domain-specific languages

2. **Performance Optimization**
   - Implement query optimizer (predicate pushdown)
   - Improve ranking algorithm
   - Optimize search ranking

3. **MCP Server**
   - Improve MCP documentation
   - Add integration tests

### Medium Priority

1. **VSCode Extension**
   - Expand integration test suite
   - Improve CodeLens functionality

2. **Documentation**
   - Expand performance tuning guide
   - Create video tutorials
   - Improve plugin development examples

3. **Testing**
   - Increase code coverage
   - Add more real-world benchmarks
   - Implement fuzzing tests

### Low Priority (Long-term)

1. **LSP Server**
   - Semantic highlighting
   - Inlay hints
   - Code lens improvements

2. **External Plugin System**
   - Plugin manifest format
   - Dynamic library loading
   - Plugin registry

---

## Questions?

- **General Questions**: Open a [GitHub Discussion](https://github.com/verivus-oss/sqry/discussions)
- **Bug Reports**: Open a [GitHub Issue](https://github.com/verivus-oss/sqry/issues)
- **Security Issues**: Email hello@sqry.dev directly
- **Feature Requests**: Start a [Discussion](https://github.com/verivus-oss/sqry/discussions) first

---

## License

All contributions are made under the [MIT License](LICENSE-MIT).

By contributing to sqry, you agree that your contributions will be licensed under the same terms.

---

Thank you for making sqry better! 🎉

---

*Last updated: March 2026 (v4.8.2)*
