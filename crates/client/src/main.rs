mod cli;
mod config;
mod changes;
mod backend;
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
    #[arg(long,action=clap::ArgAction::Set,num_args=0..=1,require_equals=true,default_missing_value="true",default_value="false")]
    desktop: bool,
    #[arg(long,help="Client config file (default: /etc/linux-assistant/config on Linux)")]
    config: Option<PathBuf>,
    #[arg(long,default_value="Linux Assistant",help="Assistant name in the browser and terminal")]
    name: String,
    #[arg(long,help="PNG logo path; replaces both the header icon and name")]
    logo: Option<PathBuf>,
    #[arg(long,conflicts_with="logo",help="PNG logo path; displays a square image beside the assistant name")]
    brand_mark: Option<PathBuf>,
    #[arg(long,help="PNG character image on the sign-in page; read from disk on each refresh")]
    assistant_image: Option<PathBuf>,
    #[arg(long,action=clap::ArgAction::Set,num_args=0..=1,require_equals=true,default_missing_value="true",default_value="false")]
    cli: bool,
    #[arg(long,hide=true,action=clap::ArgAction::Set,num_args=0..=1,require_equals=true,default_missing_value="true",default_value="false")]
    daemon: bool,
    #[arg(long,value_parser=parse_backend,help="Required backend origin; omitted scheme defaults to HTTPS (localhost HTTP is allowed)")]
    backend: String,
    #[arg(long,action=clap::ArgAction::Set,num_args=0..=1,require_equals=true,default_missing_value="true",default_value="false",help="Install a Linux desktop launcher pointing at this executable")]
    install_desktop: bool,
    #[arg(long,action=clap::ArgAction::Set,num_args=0..=1,require_equals=true,default_missing_value="true",default_value="false",help = "Reset local chat and queued input, retaining sign-in and settings; stop the daemon first")]
    reset_chat: bool,
}

/// Default launches print the reusable browser URL; interactive and browser-opening modes are explicit.
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::load()?;
    anyhow::ensure!([args.desktop,args.cli,args.daemon].into_iter().filter(|value|*value).count()<=1,"Choose only one launch mode: --desktop, --cli or --daemon");
    let brand=config::Brand::read(&args)?;
    let directory = storage::directory()?;
    if args.reset_chat { storage::reset_chat(&directory)?; println!("Local chat reset; sign-in and settings retained."); return Ok(()); }
    if args.install_desktop { return install_desktop(&args); }
    if args.daemon {
        #[cfg(windows)]
        windows_launcher::redirect_log(&directory)?;
        return local::serve(directory,&args.backend,brand).await;
    }
    let connection = local::connect(&directory,&args,&brand).await?;
    if args.desktop {
        webbrowser::open(&connection.url)?;
        return Ok(());
    }
    if args.cli { return cli::run(connection).await; }
    println!("{}",connection.url);
    Ok(())
}

/// Generate an optional freedesktop launcher without adding runtime assets beside the binary.
fn install_desktop(args:&Args) -> Result<()> {
    anyhow::ensure!(cfg!(target_os="linux"),"Desktop installation is supported on Linux");
    let directory = dirs::data_local_dir().unwrap().join("applications");
    std::fs::create_dir_all(&directory)?;
    let executable: PathBuf = std::env::current_exe()?;
    let executable = executable.to_string_lossy().replace('\\',"\\\\").replace('"',"\\\"").replace('`',"\\`").replace('$',"\\$").replace('%',"%%");
    let path = directory.join("linux-assistant.desktop");
    let name=args.name.replace('\\',"\\\\");
    let mut branding=String::new();
    for (key,value) in [("name",Some(args.name.clone())),("logo",args.logo.as_ref().map(|path|path.to_string_lossy().into_owned())),("brand-mark",args.brand_mark.as_ref().map(|path|path.to_string_lossy().into_owned())),("assistant-image",args.assistant_image.as_ref().map(|path|path.to_string_lossy().into_owned())),("config",args.config.as_ref().map(|path|path.to_string_lossy().into_owned()))] {
        if let Some(value)=value { branding.push_str(&format!(" --{key} \"{}\"",value.replace('\\',"\\\\").replace('\"',"\\\"").replace('`',"\\`").replace('$',"\\$").replace('%',"%%"))); }
    }
    let backend=args.backend.replace('%',"%%");
    std::fs::write(&path,format!("[Desktop Entry]\nType=Application\nName={name}\nComment=Ask questions and complete local tasks\nExec=\"{executable}\" --desktop --backend \"{backend}\"{branding}\nTerminal=false\nCategories=Utility;\n"))?;
    println!("Installed {}",path.display());
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
        for mode in ["--cli","--desktop","--daemon","--install-desktop"] { assert!(Args::try_parse_from(["linux-assistant",mode]).is_err()); }
        assert!(Args::try_parse_from(["linux-assistant"]).is_err());
        assert_eq!(Args::try_parse_from(["linux-assistant","--backend"," ai.slax.org/ "]).unwrap().backend,"https://ai.slax.org");
        assert_eq!(parse_backend("http://127.0.0.1:8787").unwrap(),"http://127.0.0.1:8787");
        for invalid in ["","https://","http://remote.example","https://user:pass@example.test","https://example.test/path","https://example.test?x=1"] { assert!(parse_backend(invalid).is_err(),"{invalid}"); }
    }
}
