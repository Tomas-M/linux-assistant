use super::*;
use protocol::{AuthProvider,PollLogin};

pub(super) struct LoginAttempt { pub device:DeviceLogin,pub provider:Option<AuthProvider> }

impl Engine {
    /// Own the polling secret in the native process; the popup receives only an opaque device ID.
    pub async fn social_start(&mut self,provider:AuthProvider)->Result<Value> {
        ensure!(self.saved.token.is_none(),"Sign out before starting another sign-in");
        if let Some(login)=&self.login { self.social_cancel(login.device.device_id.clone()).await?; }
        let device:DeviceLogin=Self::response(crate::backend::send(self.backend(reqwest::Method::POST,"/v1/auth/device"),&self.live,None).await?).await?;
        let result=json!({"id":device.device_id,"provider":provider,"expires_at":device.expires_at,"url":format!("{}/auth/{}/{}",self.saved.settings.backend,provider.key(),device.device_id)});
        self.login=Some(LoginAttempt { device,provider:Some(provider) });
        self.live.state(self.state());
        Ok(result)
    }

    /// Poll only the current attempt, so delayed responses from another tab cannot consume its session.
    pub async fn social_poll(&mut self,id:String)->Result<Value> {
        let login=&self.login.as_ref().context("Sign-in is no longer pending")?.device;
        ensure!(login.device_id==id,"This sign-in was replaced by another attempt");
        let input=PollLogin { device_id:id,poll_secret:login.poll_secret.clone() };
        let result:LoginResult=Self::response(crate::backend::send(self.backend(reqwest::Method::POST,"/v1/auth/poll").json(&input),&self.live,None).await?).await?;
        if result.token.is_some() { return Ok(json!({"state":self.finish_login(result).await?})); }
        Ok(json!({"email_verification":result.email_verification}))
    }

    /// Cancel by expected device ID; an old page cannot cancel a newer sign-in.
    pub async fn social_cancel(&mut self,id:String)->Result<()> {
        let Some(login)=&self.login else { return Ok(()); };
        if login.device.device_id!=id { return Ok(()); }
        let input=PollLogin { device_id:id,poll_secret:login.device.poll_secret.clone() };
        Self::response::<Value>(crate::backend::send(self.backend(reqwest::Method::POST,"/v1/auth/cancel").json(&input),&self.live,None).await?).await?;
        self.login=None;
        self.live.state(self.state());
        Ok(())
    }
}
