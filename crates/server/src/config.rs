use std::{net::SocketAddr,path::PathBuf};
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
    pub assistant_context:Option<String>,
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
            model:None,reasoning_effort:None,assistant_context:None,google_client_id:None,google_client_secret:None,microsoft_client_id:None,microsoft_client_secret:None,
            github_client_id:None,github_client_secret:None,smtp_host:"smtp.gmail.com".into(),smtp_port:465,smtp_username:None,smtp_password:None,email_from:None,
            stripe_secret:None,stripe_webhook_secret:None,credit_packs:vec![] }
    }
}

/// Production Linux deployments always keep application state in this private directory.
pub fn database_path()->PathBuf {
    if cfg!(target_os="linux") { PathBuf::from("/var/lib/linux-assistant/assistant.db") }
    else { PathBuf::from(std::env::var_os("PROGRAMDATA").unwrap_or_else(||".".into())).join("linux-assistant/assistant.db") }
}

#[derive(Debug)]
pub struct FieldError { pub field:String,pub message:String }
impl FieldError {
    /// Keep invalid setting identities separate from safe, user-facing explanations.
    pub fn new(field:&str,message:&str)->Self { Self { field:field.into(),message:message.into() } }
}
impl std::fmt::Display for FieldError {
    /// Include the setting name in startup errors without logging its contents.
    fn fmt(&self,formatter:&mut std::fmt::Formatter<'_>)->std::fmt::Result { write!(formatter,"{}: {}",self.field,self.message) }
}
impl std::error::Error for FieldError {}

/// Attach the affected form field to each configuration rule.
fn require(valid:bool,field:&str,message:&str)->Result<(),FieldError> {
    if valid { Ok(()) } else { Err(FieldError::new(field,message)) }
}

impl Config {
    /// Optional integrations may be absent, but a partially configured integration must be corrected before saving.
    pub fn validate(&self)->Result<(),FieldError> {
        require((0..=1_000_000_000).contains(&self.free_credits),"free_credits","Enter a whole number between 0 and 1000000000")?;
        let url=reqwest::Url::parse(&self.public_url).map_err(|_|FieldError::new("public_url","Enter a full URL, e.g. https://ai.slax.org"))?;
        let origin_only=url.query().is_none() && url.fragment().is_none() && url.path()=="/";
        let credentials_absent=url.username().is_empty() && url.password().is_none();
        require(origin_only && credentials_absent,"public_url","Enter only the server origin, without a path, query, fragment or credentials")?;
        require(url.scheme()=="https" || (self.dev && url.scheme()=="http"),"public_url","Public URL must use HTTPS")?;
        require(url.host_str().is_some(),"public_url","Public URL needs a hostname")?;
        require(self.model.as_ref().is_none_or(|model|!model.trim().is_empty() && model.len()<=200),"model","Enter a model of at most 200 bytes, or leave it blank")?;
        require(self.reasoning_effort.as_deref().is_none_or(|effort|["none","minimal","low","medium","high","xhigh","max","ultra","persistent"].contains(&effort)),"reasoning_effort","Choose a supported reasoning level")?;
        let hostname_valid=!self.smtp_host.is_empty() && !self.smtp_host.contains("://") && !self.smtp_host.chars().any(char::is_whitespace);
        require(hostname_valid,"smtp_host","Enter an SMTP hostname without a protocol or spaces, e.g. smtp.gmail.com")?;
        require(self.smtp_port>0,"smtp_port","Enter a port between 1 and 65535")?;
        for (field,value) in [("smtp_username",&self.smtp_username),("smtp_password",&self.smtp_password),("email_from",&self.email_from),("google_client_id",&self.google_client_id),("google_client_secret",&self.google_client_secret),("microsoft_client_id",&self.microsoft_client_id),("microsoft_client_secret",&self.microsoft_client_secret),("github_client_id",&self.github_client_id),("github_client_secret",&self.github_client_secret),("stripe_secret",&self.stripe_secret),("stripe_webhook_secret",&self.stripe_webhook_secret)] {
            if let Some(value)=value { require(!value.trim().is_empty() && value.len()<=8192,field,"Enter a nonempty value of at most 8192 bytes, or clear this setting")?; }
        }
        let email_fields=[("smtp_username",&self.smtp_username),("smtp_password",&self.smtp_password),("email_from",&self.email_from)];
        if email_fields.iter().any(|(_,value)|value.is_some()) {
            for (field,value) in email_fields { require(value.is_some(),field,"SMTP username, password and sender must all be configured together")?; }
        }
        require(!self.email_enabled || self.dev || self.smtp_username.is_some(),"smtp_username","Configure SMTP username, password and sender before enabling email sign-in")?;
        if let Some(sender)=&self.email_from {
            sender.parse::<lettre::message::Mailbox>().map_err(|_|FieldError::new("email_from","Enter an email address, optionally with a display name, e.g. Slax AI <tomas@slax.org>"))?;
        }
        for (key,name,enabled,id,secret) in [("google","Google",self.google_enabled,&self.google_client_id,&self.google_client_secret),("microsoft","Microsoft",self.microsoft_enabled,&self.microsoft_client_id,&self.microsoft_client_secret),("github","GitHub",self.github_enabled,&self.github_client_id,&self.github_client_secret)] {
            require(!enabled || id.is_some(),&format!("{key}_client_id"),&format!("Enter the {name} client ID before enabling sign-in"))?;
            let missing=if id.is_some() { "secret" } else { "id" };
            require(id.is_some()==secret.is_some(),&format!("{key}_client_{missing}"),&format!("Configure both the {name} client ID and client secret"))?;
        }
        let missing=if self.stripe_secret.is_some() { "stripe_webhook_secret" } else { "stripe_secret" };
        require(self.stripe_secret.is_some()==self.stripe_webhook_secret.is_some(),missing,"Configure both the Stripe secret API key and webhook signing secret")?;
        let mut prices=std::collections::HashSet::new();
        for (index,pack) in self.credit_packs.iter().enumerate() {
            let field=format!("credit_packs.{index}");
            let price_valid=pack.price_id.starts_with("price_") && pack.price_id.len()<=200 && pack.price_id.bytes().all(|byte|byte.is_ascii_alphanumeric() || byte==b'_');
            require(price_valid,&format!("{field}.price_id"),"Enter a Stripe Price ID starting with price_, using only letters, digits and underscores (at most 200 bytes)")?;
            require(prices.insert(&pack.price_id),&format!("{field}.price_id"),"Each credit pack must have a different Stripe Price ID")?;
            require((1..=1_000_000_000).contains(&pack.credits),&format!("{field}.credits"),"Enter a whole number between 1 and 1000000000")?;
            pack.cents().map_err(|_|FieldError::new(&format!("{field}.usd"),"Enter a positive USD amount with at most two decimal places"))?;
        }
        Ok(())
    }
}
