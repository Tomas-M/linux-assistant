mod cli;
mod session;
mod config;
mod changes;
mod files;
mod downloads;
mod media;
mod backend;
mod controls;
mod command;
mod processes;
mod engine;
mod local;
mod ui;
mod stream;
#[cfg(windows)]
mod windows_launcher;

use std::path::PathBuf;
use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(version,about="A general-purpose assistant for Linux and Windows, in your terminal or browser")]
struct Args {
    #[arg(long,help="Client config file (default: /etc/linux-assistant/config on Linux)")]
    config:Option<PathBuf>,
    #[arg(long)]
    cli:bool,
    #[arg(long,hide=true)]
    daemon:bool,
    #[arg(long,value_parser=parse_backend,help="Backend origin; omitted scheme defaults to HTTPS")]
    backend:String,
}

/// Default launches print the reusable browser URL; --cli selects the terminal interface.
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::load()?;
    anyhow::ensure!(!args.cli || !args.daemon,"Choose either terminal mode or server mode");
    if args.cli { return cli::run(&args.backend).await; }
    if args.daemon { return local::serve(&args.backend).await; }
    let connection=local::connect(&args).await?;
    println!("{}",connection.url);
    Ok(())
}

/// Validate the backend before creating local state or starting a daemon.
fn parse_backend(value:&str)->std::result::Result<String,String> { engine::normalize_backend(value).map_err(|error|error.to_string()) }

#[cfg(test)]
mod tests {
    use super::*;

    /// Every launch mode requires a backend, and scheme-free origins normalize before daemon reuse.
    #[test]
    fn requires_and_normalizes_backend() {
        for mode in ["--cli","--daemon"] { assert!(Args::try_parse_from(["linux-assistant",mode]).is_err()); }
        assert!(Args::try_parse_from(["linux-assistant"]).is_err());
        assert_eq!(Args::try_parse_from(["linux-assistant","--backend"," ai.slax.org/ "]).unwrap().backend,"https://ai.slax.org");
        assert_eq!(parse_backend("http://127.0.0.1:8787").unwrap(),"http://127.0.0.1:8787");
        for invalid in ["","https://","http://remote.example","https://user:pass@example.test","https://example.test/path","https://example.test?x=1"] { assert!(parse_backend(invalid).is_err(),"{invalid}"); }
    }
}
