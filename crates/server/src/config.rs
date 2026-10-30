use std::{net::SocketAddr,path::PathBuf};
use anyhow::{ensure,Result};
use clap::Parser;
use serde::{Deserialize,Serialize};

#[derive(Parser)]
#[command(about="Linux Assistant backend; configure at /admin")]
pub struct Args {
    #[arg(long,help="Local-only simulated AI and email")]
    pub dev:bool,
    #[arg(long,requires="dev",help="Development database override")]
    pub database:Option<PathBuf>,
    #[arg(long,requires="dev",help="Development listener override")]
    pub listen:Option<SocketAddr>,
    #[arg(long,hide=true)]
    pub provider_worker:bool,
    #[arg(long)]
    pub third_party_notices:bool,
    #[arg(long)]
    pub codex_version:bool,
}

#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(skip)]
    pub listen:Option<SocketAddr>,
    #[serde(skip)]
    pub database:PathBuf,
    #[serde(skip)]
    pub dev:bool,
    #[serde(skip)]
    pub admin_password_hash:Option<String>,
    #[cfg(test)]
    #[serde(skip)]
    pub test_homes:Vec<PathBuf>,
    pub public_url:String,
    pub email_enabled:bool,
    pub google_enabled:bool,
    pub microsoft_enabled:bool,
    pub github_enabled:bool,
    pub free_credits:i64,
    pub pricing:crate::billing::Pricing,
    pub model:Option<String>,
    pub reasoning_effort:Option<String>,
    pub google_client_id:Option<String>,
    pub google_client_secret:Option<String>,
    pub microsoft_client_id:Option<String>,
    pub microsoft_client_secret:Option<String>,
    pub github_client_id:Option<String>,
    pub github_client_secret:Option<String>,
    pub smtp_host:String,
    pub smtp_port:u16,
    pub smtp_username:Option<String>,
    pub smtp_password:Option<String>,
    pub email_from:Option<String>,
    pub stripe_secret:Option<String>,
    pub stripe_webhook_secret:Option<String>,
    pub credit_packs:Vec<protocol::CreditPack>,
}

impl Default for Config {
    /// Fresh deployments start with no credentials and use only the browser setup flow.
    fn default()->Self {
        Self { listen:None,database:database_path(),dev:false,admin_password_hash:None,
            #[cfg(test)] test_homes:vec![],public_url:String::new(),email_enabled:false,google_enabled:false,microsoft_enabled:false,github_enabled:false,free_credits:100,
            pricing:crate::billing::Pricing { input_credits_per_million:crate::billing::Rate(200_000_000),cached_input_credits_per_million:crate::billing::Rate(10_000_000),output_credits_per_million:crate::billing::Rate(1_000_000_000) },
            model:None,reasoning_effort:None,google_client_id:None,google_client_secret:None,microsoft_client_id:None,microsoft_client_secret:None,
            github_client_id:None,github_client_secret:None,smtp_host:"smtp.gmail.com".into(),smtp_port:465,smtp_username:None,smtp_password:None,email_from:None,
            stripe_secret:None,stripe_webhook_secret:None,credit_packs:vec![] }
    }
}

/// Production Linux deployments always keep application state in this private directory.
pub fn database_path()->PathBuf {
    if cfg!(target_os="linux") { PathBuf::from("/var/lib/linux-assistant/assistant.db") }
    else { PathBuf::from(std::env::var_os("PROGRAMDATA").unwrap_or_else(||".".into())).join("linux-assistant/assistant.db") }
}

impl Config {
    /// Optional integrations may be absent, but a partially configured integration must be corrected before saving.
    pub fn validate(&self)->Result<()> {
        ensure!(self.free_credits>=0 && self.free_credits<=1_000_000_000,"Invalid welcome credit amount");
        let url=reqwest::Url::parse(&self.public_url)?;
        let origin_only=url.query().is_none() && url.fragment().is_none() && url.path()=="/";
        let credentials_absent=url.username().is_empty() && url.password().is_none();
        ensure!(origin_only && credentials_absent,"Public URL must be an origin without credentials");
        ensure!(url.scheme()=="https" || (self.dev && url.scheme()=="http"),"Public URL must use HTTPS");
        ensure!(url.host_str().is_some(),"Public URL needs a hostname");
        ensure!(self.model.as_ref().is_none_or(|model|!model.trim().is_empty() && model.len()<=200),"Enter a model or leave it blank");
        ensure!(self.reasoning_effort.as_deref().is_none_or(|effort|["none","minimal","low","medium","high","xhigh","max","ultra","persistent"].contains(&effort)),"Unsupported reasoning level");
        let hostname_valid=!self.smtp_host.is_empty() && !self.smtp_host.contains("://") && !self.smtp_host.chars().any(char::is_whitespace);
        ensure!(hostname_valid && self.smtp_port>0,"Enter an SMTP hostname and a nonzero port");
        for value in [&self.smtp_username,&self.smtp_password,&self.email_from,&self.google_client_id,&self.google_client_secret,&self.microsoft_client_id,&self.microsoft_client_secret,&self.github_client_id,&self.github_client_secret,&self.stripe_secret,&self.stripe_webhook_secret].into_iter().flatten() {
            ensure!(!value.trim().is_empty() && value.len()<=8192,"Credential fields must be nonempty and at most 8192 bytes");
        }
        let email_fields=[self.smtp_username.is_some(),self.smtp_password.is_some(),self.email_from.is_some()];
        ensure!(email_fields.iter().all(|value|*value) || email_fields.iter().all(|value|!*value),"Configure SMTP username, password and sender together, or clear all three");
        ensure!(!self.email_enabled || self.dev || self.smtp_username.is_some(),"Configure SMTP before enabling email sign-in");
        if let Some(sender)=&self.email_from { sender.parse::<lettre::message::Mailbox>()?; }
        for (name,enabled,id,secret) in [("Google",self.google_enabled,&self.google_client_id,&self.google_client_secret),("Microsoft",self.microsoft_enabled,&self.microsoft_client_id,&self.microsoft_client_secret),("GitHub",self.github_enabled,&self.github_client_id,&self.github_client_secret)] {
            ensure!(!enabled || id.is_some(),"Configure {name} credentials before enabling sign-in");
            ensure!(id.is_some()==secret.is_some(),"Configure both {name} credentials");
        }
        ensure!(self.stripe_secret.is_some()==self.stripe_webhook_secret.is_some(),"Configure both Stripe secrets");
        let mut prices=std::collections::HashSet::new();
        for pack in &self.credit_packs {
            ensure!(pack.price_id.starts_with("price_") && pack.price_id.len()<=200 && pack.price_id.bytes().all(|byte|byte.is_ascii_alphanumeric() || byte==b'_'),"Enter a valid Stripe Price ID");
            ensure!(prices.insert(&pack.price_id),"Each credit pack must have a different Price ID");
            ensure!((1..=1_000_000_000).contains(&pack.credits),"Pack credits must be between 1 and 1000000000");
            pack.cents()?;
        }
        Ok(())
    }
}
