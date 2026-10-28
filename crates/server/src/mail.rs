use std::time::Duration;
use anyhow::Result;
use lettre::{Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, header::ContentType}, transport::smtp::{AsyncSmtpTransportBuilder, authentication::Credentials}};
use crate::{App, config::Config};

/// Use authenticated SMTP with TLS from connection start and certificate validation.
pub fn transport(config: &Config) -> Result<AsyncSmtpTransportBuilder> {
    Ok(AsyncSmtpTransport::<Tokio1Executor>::relay(&config.smtp_host)?
        .port(config.smtp_port)
        .credentials(Credentials::new(config.smtp_username.clone().unwrap(),config.smtp_password.clone().unwrap()))
        .timeout(Some(Duration::from_secs(30))))
}

/// Deliver the sign-in code with a bound on the entire SMTP exchange.
pub async fn send_code(app: &App, recipient: Address, code: &str) -> Result<()> {
    let message=Message::builder().from(app.config.email_from.clone().unwrap())
        .to(Mailbox::new(None,recipient)).subject("Your Linux Assistant sign-in code")
        .header(ContentType::TEXT_PLAIN)
        .body(format!("Your sign-in code is {code}. It expires in 10 minutes. Only enter it if you started sign-in in Linux Assistant."))?;
    tokio::time::timeout(Duration::from_secs(30),app.mail.as_ref().unwrap().send(message)).await??;
    Ok(())
}
