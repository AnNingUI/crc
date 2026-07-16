//! CR 编译器主程序

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let cli = crc_lib::cli::Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(if cli.verbose { "debug" } else { "info" })
        .init();
    crc_lib::cli::run(cli)
}
