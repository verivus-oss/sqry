use anyhow::Result;
use clap::Parser;
use sqry_lsp::{LspCli, run};

fn main() -> Result<()> {
    let cli = LspCli::parse();
    run(cli.into_options())
}
