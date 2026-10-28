mod cli;
mod controls;
mod command;
mod processes;
mod engine;
mod local;
mod storage;
mod stream;
#[cfg(windows)]
mod windows_launcher;

use std::path::PathBuf;
use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(version,about="A general-purpose assistant for Linux and Windows, in your terminal or browser")]
struct Args {
    #[arg(long,conflicts_with_all=["cli","daemon"])]
    desktop: bool,
    #[arg(long,conflicts_with="daemon")]
    cli: bool,
    #[arg(long,hide=true)]
    daemon: bool,
    #[arg(long,help="Save an HTTPS backend origin (localhost HTTP is allowed)")]
    backend: Option<String>,
    #[arg(long,help="Install a Linux desktop launcher pointing at this executable")]
    install_desktop: bool,
    #[arg(long,help = "Reset local chat and queued input, retaining sign-in and settings; stop the daemon first")]
    reset_chat: bool,
}

/// Default launches print the reusable browser URL; interactive and browser-opening modes are explicit.
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let directory = storage::directory()?;
    if args.reset_chat { storage::reset_chat(&directory)?; println!("Local chat reset; sign-in and settings retained."); return Ok(()); }
    if args.install_desktop { return install_desktop(); }
    if args.daemon {
        #[cfg(windows)]
        windows_launcher::redirect_log(&directory)?;
        return local::serve(directory).await;
    }
    let connection = local::connect(&directory).await?;
    if let Some(backend) = args.backend { connection.post("/api/backend",serde_json::json!({"backend":backend})).await?; }
    if args.desktop {
        webbrowser::open(&format!("{}/#{}",connection.url,connection.secret))?;
        return Ok(());
    }
    if args.cli { return cli::run(connection).await; }
    println!("{}/#{}",connection.url,connection.secret);
    Ok(())
}

/// Generate an optional freedesktop launcher without adding runtime assets beside the binary.
fn install_desktop() -> Result<()> {
    anyhow::ensure!(cfg!(target_os="linux"),"Desktop installation is supported on Linux");
    let directory = dirs::data_local_dir().unwrap().join("applications");
    std::fs::create_dir_all(&directory)?;
    let executable: PathBuf = std::env::current_exe()?;
    let executable = executable.to_string_lossy().replace('\\',"\\\\").replace('"',"\\\"").replace('`',"\\`").replace('$',"\\$").replace('%',"%%");
    let path = directory.join("linux-assistant.desktop");
    std::fs::write(&path,format!("[Desktop Entry]\nType=Application\nName=Linux Assistant\nComment=Ask questions and complete local tasks\nExec=\"{executable}\" --desktop\nTerminal=false\nCategories=Utility;\n"))?;
    println!("Installed {}",path.display());
    Ok(())
}
