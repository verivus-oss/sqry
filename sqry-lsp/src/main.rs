#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use anyhow::Result;
use clap::Parser;
use sqry_lsp::{LspCli, run};

fn main() -> Result<()> {
    let cli = LspCli::parse();
    run(cli.into_options())
}
