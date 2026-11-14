use std::{ffi::OsString,path::{Path,PathBuf},sync::{Arc,RwLock}};
use anyhow::{ensure,Context,Result};
use clap::Parser;
use serde_json::{json,Value};

impl crate::Args {
    /// Only the backend URL belongs in the client configuration file.
    pub fn load()->Result<Self> {
        #[cfg(unix)]
        let default=PathBuf::from("/etc/linux-assistant/config");
        #[cfg(windows)]
        let default=PathBuf::from(std::env::var_os("ProgramData").unwrap_or_else(||"C:\\ProgramData".into())).join("linux-assistant/config");
        Self::load_from(std::env::args_os().collect(),&default)
    }
    /// Read the backend default, giving an explicit command-line URL precedence.
    fn load_from(arguments:Vec<OsString>,default:&Path)->Result<Self> {
        if arguments.iter().any(|arg|arg=="--help" || arg=="--version" || arg=="-h" || arg=="-V") { return Ok(Self::parse_from(arguments)); }
        let explicit=arguments.iter().position(|arg|arg=="--config").map(|index|arguments.get(index+1).map(PathBuf::from).context("--config requires a path")).transpose()?;
        let explicit=explicit.or_else(||arguments.iter().find_map(|arg|arg.to_str()?.strip_prefix("--config=").map(PathBuf::from)));
        let path=explicit.as_deref().unwrap_or(default);
        let contents=match std::fs::read_to_string(path) { Ok(text)=>text,Err(error) if error.kind()==std::io::ErrorKind::NotFound && explicit.is_none()=>String::new(),Err(error)=>return Err(error).with_context(||format!("Cannot read {}",path.display())) };
        let values:toml::Table=toml::from_str(&contents)?;
        let mut merged=vec![arguments[0].clone()];
        for (key,value) in values {
            ensure!(key=="backend","Unknown client config option: {key}. Only backend is supported");
            let value=value.as_str().context("backend must be text")?;
            if !arguments.iter().skip(1).any(|arg|arg.to_str().is_some_and(|arg|arg==format!("--{key}") || arg.starts_with(&format!("--{key}=")))) { merged.push(format!("--{key}={value}").into()); }
        }
        merged.extend(arguments.into_iter().skip(1));
        let mut args=Self::try_parse_from(merged)?;
        if explicit.is_some() { args.config=Some(std::fs::canonicalize(path)?); }
        Ok(args)
    }
    /// The child receives the resolved backend; terminal mode is never inherited.
    pub fn daemon_args(&self)->Vec<OsString> {
        let mut args=vec!["--daemon".into(),"--backend".into(),self.backend.clone().into()];
        if let Some(path)=&self.config { args.extend(["--config".into(),path.clone().into_os_string()]); }
        args
    }
}

#[derive(Default)]
pub struct Brand { document:RwLock<Option<Value>>,providers:RwLock<Value>,loading:tokio::sync::Mutex<()> }
impl Brand {
    /// Sign-in options share the startup snapshot and require no browser-triggered backend fetch.
    pub fn providers(&self)->Value { self.providers.read().unwrap().clone() }
    /// The manifest contains only public names, theme and image hashes, never image bodies.
    pub fn metadata(&self)->Value { self.document.read().unwrap().clone().unwrap_or(Value::Null) }
}

/// Fetch once at startup; retry an unsuccessful startup only when explicitly requested, never poll settings.
pub async fn load(backend:String,brand:Arc<Brand>,live:crate::stream::Live) {
    let Ok(_loading)=brand.loading.try_lock() else { return; };
    if brand.document.read().unwrap().is_some() { return; }
    live.0.send_modify(|view|view["backend_error"]=Value::Null);
    let result=async {
        let response=crate::backend::send(crate::backend::client(60)?.get(format!("{backend}/v1/client-config")),&live,None).await?;
        ensure!(response.status().is_success(),"Backend configuration unavailable (HTTP {}). Update the backend and reconnect",response.status());
        let document:Value=response.json().await?;
        let name=document["name"].as_str().context("Assistant name is missing")?;
        ensure!(!name.trim().is_empty() && name.len()<=100 && !name.chars().any(char::is_control),"Invalid assistant name from backend");
        ensure!(document["images"].as_object().context("Brand image manifest is missing")?.values().all(|hash|hash.as_str().is_some_and(|hash|hash.len()==64 && hash.bytes().all(|byte|byte.is_ascii_hexdigit()))),"Invalid image manifest from backend");
        let response=crate::backend::send(crate::backend::client(60)?.get(format!("{backend}/v1/auth/providers")),&live,None).await?;
        ensure!(response.status().is_success(),"Cannot load sign-in options (HTTP {})",response.status());
        *brand.providers.write().unwrap()=response.json().await?;
        *brand.document.write().unwrap()=Some(document.clone());
        live.0.send_modify(|view| { view["brand"]=json!(document); view["backend_error"]=Value::Null; });
        Ok::<_,anyhow::Error>(())
    }.await;
    if let Err(error)=result { live.0.send_modify(|view|view["backend_error"]=json!(error.to_string())); }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The file configures only the backend; terminal mode must be explicitly selected on the command line.
    #[test]
    fn defaults_and_overrides()->Result<()> {
        let directory=tempfile::tempdir()?;let path=directory.path().join("config");
        std::fs::write(&path,"backend='ai.slax.org'\n")?;
        let args=crate::Args::load_from(vec!["assistant".into()],&path)?;
        assert!(!args.cli); assert_eq!(args.backend,"https://ai.slax.org");
        let args=crate::Args::load_from(vec!["assistant".into(),"--cli".into()],&path)?;assert!(args.cli);
        let mut child=vec!["assistant".into()];child.extend(args.daemon_args());
        assert!(crate::Args::load_from(child,&path)?.daemon);
        std::fs::write(&path,"backend='ai.slax.org'\ncli=true\n")?;
        assert!(crate::Args::load_from(vec!["assistant".into()],&path).is_err());
        Ok(())
    }
}
