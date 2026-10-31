use std::{ffi::OsString,path::{Path,PathBuf}};
use anyhow::{ensure,Context,Result};
use clap::{CommandFactory,Parser};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};

impl crate::Args {
    /// Read system defaults first; explicitly supplied options and launch modes take precedence.
    pub fn load()->Result<Self> {
        let arguments:Vec<OsString>=std::env::args_os().collect();
        #[cfg(unix)]
        let default=PathBuf::from("/etc/linux-assistant/config");
        #[cfg(windows)]
        let default=std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(||PathBuf::from(r"C:\ProgramData")).join("linux-assistant/config");
        Self::load_from(arguments,&default)
    }

    /// Parse config through the CLI schema, so unknown keys and invalid values fail consistently.
    fn load_from(arguments:Vec<OsString>,default:&Path)->Result<Self> {
        let supplied:Vec<_>=arguments.iter().skip(1).filter_map(|argument|argument.to_str()?.strip_prefix("--").map(|key|key.split('=').next().unwrap())).collect();
        if supplied.iter().any(|key|matches!(*key,"help"|"version")) { return Ok(Self::parse_from(arguments)); }
        let explicit=arguments.iter().position(|argument|argument=="--config").map(|index|arguments.get(index+1).map(PathBuf::from).context("--config requires a path")).transpose()?;
        let explicit=explicit.or_else(||arguments.iter().find_map(|argument|argument.to_str()?.strip_prefix("--config=").map(PathBuf::from)));
        let path=explicit.as_deref().unwrap_or(default);
        let contents=match std::fs::read_to_string(path) {
            Ok(text)=>text,
            Err(error) if error.kind()==std::io::ErrorKind::NotFound && explicit.is_none()=>String::new(),
            Err(error)=>return Err(error).with_context(||format!("Cannot read {}",path.display())),
        };
        let values:toml::Table=toml::from_str(&contents).with_context(||format!("Invalid config {}",path.display()))?;
        let command=Self::command();
        let mode=supplied.iter().any(|key|matches!(*key,"desktop"|"cli"|"daemon"));
        let mut merged=vec![arguments[0].clone()];
        for (key,value) in values {
            ensure!(key!="config" && command.get_arguments().any(|argument|argument.get_long()==Some(&key)),"Unknown client config option: {key}");
            if supplied.contains(&key.as_str()) || (mode && matches!(key.as_str(),"desktop"|"cli"|"daemon")) { continue; }
            let value=match value { toml::Value::String(value)=>value,toml::Value::Boolean(value)=>value.to_string(),_=>anyhow::bail!("Config option {key} must be a string or boolean") };
            merged.push(format!("--{key}={value}").into());
        }
        merged.extend(arguments.into_iter().skip(1));
        let mut args=Self::try_parse_from(merged)?;
        if explicit.is_some() { args.config=Some(std::fs::canonicalize(path)?); }
        ensure!(!args.name.trim().is_empty() && args.name.len()<=100 && !args.name.chars().any(char::is_control),"Name must be 1–100 bytes without control characters");
        if let Some(logo)=&args.logo { args.logo=Some(std::fs::canonicalize(logo).context("Cannot find the PNG logo")?); }
        if let Some(mark)=&args.brand_mark { args.brand_mark=Some(std::fs::canonicalize(mark).context("Cannot find the PNG brand mark")?); }
        Ok(args)
    }

    /// Pass resolved values to the detached child, independent of the launcher's original working directory.
    pub fn daemon_args(&self)->Vec<OsString> {
        let mut args=vec!["--daemon".into(),"--desktop=false".into(),"--cli=false".into(),"--reset-chat=false".into(),"--install-desktop=false".into(),"--backend".into(),self.backend.clone().into(),"--name".into(),self.name.clone().into()];
        if let Some(path)=&self.config { args.extend(["--config".into(),path.clone().into_os_string()]); }
        if let Some(path)=&self.logo { args.extend(["--logo".into(),path.clone().into_os_string()]); }
        if let Some(path)=&self.brand_mark { args.extend(["--brand-mark".into(),path.clone().into_os_string()]); }
        args
    }
}

#[derive(Default)]
pub struct Brand { pub name:String,pub logo:Option<Vec<u8>>,pub mark:bool }
impl Brand {
    /// Read a bounded, actual PNG once, never expose the arbitrary filesystem path over HTTP.
    pub fn read(args:&crate::Args)->Result<Self> {
        let logo=if let Some(path)=args.logo.as_ref().or(args.brand_mark.as_ref()) {
            use std::io::Read;
            let mut bytes=Vec::new();
            std::fs::File::open(path)?.take(1024*1024+1).read_to_end(&mut bytes)?;
            ensure!(bytes.len()<=1024*1024 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len()>=24,"Logo must be a PNG up to 1 MiB");
            let width=u32::from_be_bytes(bytes[16..20].try_into().unwrap());
            let height=u32::from_be_bytes(bytes[20..24].try_into().unwrap());
            ensure!(width>0 && height>0 && width<=4096 && height<=4096,"Logo must be at most 4096 × 4096 pixels");
            Some(bytes)
        } else { None };
        Ok(Self { name:args.name.clone(),logo,mark:args.brand_mark.is_some() })
    }
    /// Fingerprint bytes to detect a changed logo when reusing an existing daemon.
    pub fn metadata(&self)->Value {
        json!({"name":self.name,"logo":self.logo.as_ref().map(|bytes|format!("{:x}",Sha256::digest(bytes))),"mark":self.mark})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Config supplies the required backend, preserves Unicode names, and yields to CLI modes/values.
    #[test]
    fn defaults_and_overrides()->Result<()> {
        let directory=tempfile::tempdir()?;
        let path=directory.path().join("config");
        std::fs::write(&path,"# Defaults\nbackend='ai.slax.org'\nname='Slax 世界'\ndesktop=true\n")?;
        let args=crate::Args::load_from(vec!["assistant".into(),"--cli".into(),"--name=Slax AI".into()],&path)?;
        assert_eq!(args.backend,"https://ai.slax.org"); assert_eq!(args.name,"Slax AI"); assert!(args.cli && !args.desktop);
        let args=crate::Args::load_from(vec!["assistant".into()],&path)?;
        assert!(args.desktop); assert_eq!(args.name,"Slax 世界");
        let mut child=vec![OsString::from("assistant")]; child.extend(args.daemon_args());
        assert!(crate::Args::load_from(child,&path)?.daemon);
        std::fs::write(&path,"bakend='typo'")?;
        assert!(crate::Args::load_from(vec!["assistant".into()],&path).is_err());
        Ok(())
    }
}
