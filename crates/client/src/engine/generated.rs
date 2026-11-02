use super::*;

impl Engine {
    /// Download generated originals once; chat snapshots and persisted presentation contain only local references.
    pub(super) async fn cache_generated(&mut self,progress:&mut protocol::Progress)->Result<()> {
        for part in &mut progress.summaries {
            let Some(image)=part.data.as_ref().and_then(|data|data.get("image")).filter(|image|!image.is_null()) else { continue; };
            let id=Uuid::parse_str(image["id"].as_str().context("Generated image ID is missing")?)?.to_string();
            if self.live.0.borrow()["tool_results"][&part.id]["image"]["generated"]==true { continue; }
            let name=image["name"].as_str().unwrap_or("Generated image.png").to_owned();
            let directory=self.path.parent().unwrap().to_owned();
            let metadata=directory.join("media").join(format!("{id}-0.json"));
            let result=async {
                if !metadata.exists() {
                    let mut response=self.send_authenticated(self.backend(reqwest::Method::GET,&format!("/v1/images/{id}")),false).await?;
                    ensure!(response.status().is_success(),"Generated image is unavailable (HTTP {})",response.status());
                    let mut bytes=Vec::new();
                    while let Some(chunk)=response.chunk().await? {
                        ensure!(bytes.len()+chunk.len()<=32*1024*1024,"Generated image exceeds 32 MiB");
                        bytes.extend_from_slice(&chunk);
                    }
                    let cache=directory.clone(); let scope=id.clone();
                    tokio::task::spawn_blocking(move||crate::media::store(&cache,&scope,0,&bytes)).await??;
                }
                let stored:Value=storage::read(&metadata)?;
                let path=directory.join("media").join(format!("{}.image",stored["hash"].as_str().unwrap()));
                self.live.0.send_modify(|view| {
                    let run=view["runs"].as_array().unwrap().last().unwrap()["started_at"].clone();
                    view["tool_results"][&part.id]=json!({"success":true,"output":format!("Generated image saved locally to {}",path.display()),
                        "image":{"kind":"image","name":name,"url":format!("/api/media/{id}/0"),"generated":true,"path":path,"run":run}});
                });
                Ok::<_,anyhow::Error>(())
            }.await;
            if let Err(error)=result {
                if error.downcast_ref::<crate::backend::SessionExpired>().is_some() { return Err(error); }
                part.text=format!("Generated image could not be downloaded: {error}");
            }
        }
        Ok(())
    }
}
