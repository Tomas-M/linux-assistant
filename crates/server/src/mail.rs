use std::time::Duration;
use anyhow::Result;
use lettre::{Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, MultiPart, SinglePart}, transport::smtp::{AsyncSmtpTransportBuilder, authentication::Credentials}};
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
        .multipart(MultiPart::alternative()
            .singlepart(SinglePart::plain(format!("Sign in to Linux Assistant\n\nEnter this code in your assistant:\n\n{code}\n\nThis code expires in 10 minutes and can only be used once.\nIf you did not request it, you can ignore this email.\n\nThe Linux Assistant team")))
            .singlepart(SinglePart::html(code_html(code))))?;
    tokio::time::timeout(Duration::from_secs(30),app.mail.as_ref().unwrap().send(message)).await??;
    Ok(())
}

/// Adjacent inline spans add visual spacing without putting whitespace into the copied code.
fn code_html(code:&str)->String {
    format!(r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><body style="margin:0;padding:32px 16px;background:#f4f7f5;color:#242a27;font:16px/1.6 Arial,sans-serif"><div style="max-width:480px;margin:0 auto;padding:32px;background:#fff;border:1px solid #e3eae5;border-radius:16px"><p style="margin:0 0 24px;color:#23764e;font-weight:bold">Linux Assistant</p><h2 style="font-size:24px;line-height:1.2;margin:0 0 16px">Your sign-in code</h2><p>Welcome! Enter the code below in your assistant to finish signing in.</p><h1 style="font-size:36px;line-height:1.2;font-variant-numeric:tabular-nums;letter-spacing:2px;margin:28px 0;color:#23764e;user-select:all;-webkit-user-select:all"><span>{}</span><span style="margin-left:0.35em">{}</span></h1><p>This code expires in <strong>10 minutes</strong> and can only be used once.</p><p style="color:#69756d;font-size:14px;margin:24px 0 0">If you did not request this code, you can safely ignore this email.</p></div></body></html>"#,&code[..3],&code[3..])
}
