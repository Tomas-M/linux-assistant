use std::{net::SocketAddr, path::PathBuf};
use anyhow::{ensure, Result};
use clap::Parser;

#[derive(Clone, Parser)]
#[command(about = "Linux Assistant account, credit, and AI backend")]
pub struct Config {
    #[arg(long, env = "ASSISTANT_LISTEN", default_value = "127.0.0.1:8787")]
    pub listen: SocketAddr,
    #[arg(long, env = "ASSISTANT_PUBLIC_URL", default_value = "http://127.0.0.1:8787")]
    pub public_url: String,
    #[arg(long, env = "ASSISTANT_DATABASE", default_value = "assistant.db")]
    pub database: PathBuf,
    #[arg(long, help = "Local-only fake AI and email sign-in; never enables real payments")]
    pub dev: bool,
    #[arg(long, env = "ASSISTANT_FREE_CREDITS", default_value_t = 100)]
    pub free_credits: i64,
    #[command(flatten)]
    pub pricing: crate::billing::Pricing,
    #[arg(long, env = "ASSISTANT_ADMIN_PASSWORD", hide_env_values = true)]
    pub admin_password: Option<String>,
    #[arg(long, hide = true)]
    pub provider_worker: bool,
    #[arg(long, value_name = "DIRECTORY", help = "Sign one operator account into an isolated directory, then exit")]
    pub login_account: Option<PathBuf>,
    #[arg(long, help = "Print the embedded Codex license and notices, then exit")]
    pub third_party_notices: bool,
    #[arg(long, help = "Print the source-linked Codex App Server version, then exit")]
    pub codex_version: bool,
    #[arg(long, env = "ASSISTANT_CODEX_HOMES", value_delimiter = ',')]
    pub codex_homes: Vec<PathBuf>,
    #[arg(long, env = "ASSISTANT_MODEL")]
    pub model: Option<String>,
    #[arg(long, env = "ASSISTANT_REASONING_EFFORT", value_parser = ["none","minimal","low","medium","high","xhigh","max","ultra","persistent"],
        help = "Reasoning level for every AI turn; choose a level supported by the selected model")]
    pub reasoning_effort: Option<String>,
    #[arg(long, env = "GOOGLE_CLIENT_ID")]
    pub google_client_id: Option<String>,
    #[arg(long, env = "GOOGLE_CLIENT_SECRET", hide_env_values = true)]
    pub google_client_secret: Option<String>,
    #[arg(long, env = "ASSISTANT_SMTP_HOST", default_value = "smtp.gmail.com", help = "SMTP hostname without a URL scheme; connections use implicit TLS")]
    pub smtp_host: String,
    #[arg(long, env = "ASSISTANT_SMTP_PORT", default_value_t = 465)]
    pub smtp_port: u16,
    #[arg(long, env = "ASSISTANT_SMTP_USERNAME")]
    pub smtp_username: Option<String>,
    #[arg(long, env = "ASSISTANT_SMTP_PASSWORD", hide_env_values = true)]
    pub smtp_password: Option<String>,
    #[arg(long, env = "ASSISTANT_EMAIL_FROM")]
    pub email_from: Option<lettre::message::Mailbox>,
    #[arg(long, env = "STRIPE_SECRET_KEY", hide_env_values = true)]
    pub stripe_secret: Option<String>,
    #[arg(long, env = "STRIPE_WEBHOOK_SECRET", hide_env_values = true)]
    pub stripe_webhook_secret: Option<String>,
    #[arg(long, env = "STRIPE_PRICE_ID")]
    pub stripe_price: Option<String>,
    #[arg(long, env = "ASSISTANT_PACK_CREDITS", default_value_t = 100)]
    pub pack_credits: i64,
}

impl Config {
    /// Reject development exposure and incomplete provider configuration at startup.
    pub fn validate(&self) -> Result<()> {
        ensure!(self.free_credits >= 0 && self.pack_credits > 0, "Invalid credit amounts");
        ensure!(self.admin_password.as_ref().is_none_or(|value|value.len()>=16), "Admin password must contain at least 16 bytes");
        let url = reqwest::Url::parse(&self.public_url)?;
        ensure!(url.query().is_none() && url.fragment().is_none() && url.path() == "/", "Public URL must be an origin");
        if self.dev {
            ensure!(self.listen.ip().is_loopback(), "Development mode must bind to loopback");
            ensure!(matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")), "Development URL must be localhost");
        } else {
            ensure!(url.scheme() == "https", "Production public URL must use HTTPS (reverse proxy)");
            ensure!(!self.codex_homes.is_empty(), "Configure dedicated Codex account homes");
            ensure!(!self.smtp_host.trim().is_empty() && !self.smtp_host.contains("://"), "Set ASSISTANT_SMTP_HOST to a hostname without ssl:// or smtps://");
            ensure!(self.smtp_port>0, "SMTP port must be nonzero");
            ensure!(self.smtp_username.as_ref().is_some_and(|value|!value.trim().is_empty()), "Configure ASSISTANT_SMTP_USERNAME");
            ensure!(self.smtp_password.as_ref().is_some_and(|value|!value.trim().is_empty()), "Configure ASSISTANT_SMTP_PASSWORD");
            ensure!(self.email_from.is_some(), "Configure ASSISTANT_EMAIL_FROM");
        }
        ensure!(self.google_client_id.is_some() == self.google_client_secret.is_some(), "Configure both Google credentials");
        let payment_fields = [self.stripe_secret.is_some(), self.stripe_webhook_secret.is_some(), self.stripe_price.is_some()];
        ensure!(payment_fields.iter().all(|v| *v) || payment_fields.iter().all(|v| !*v), "Configure all three Stripe settings");
        Ok(())
    }
}
