use anyhow::{ensure,Result};
use serde::{Deserialize,Serialize};

/// Prepared for a future selector: requests have no effect while client model selection is disabled.
#[derive(Clone,Debug,Default,PartialEq,Deserialize,Serialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationOptions { pub model:Option<String>,pub reasoning_effort:Option<String> }
impl GenerationOptions {
    /// Bound identifiers at both request boundaries without baking model names into either executable.
    pub fn validate(&self)->Result<()> {
        for value in [&self.model,&self.reasoning_effort].into_iter().flatten() {
            ensure!(!value.is_empty() && value.len()<=200 && !value.chars().any(|character|character.is_control() || character.is_whitespace()),"Model and reasoning identifiers must be 1 to 200 bytes without whitespace or control characters");
        }
        Ok(())
    }
}

#[derive(Clone,Debug,PartialEq,Deserialize,Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelOption { pub id:String,pub reasoning_levels:Vec<String> }

/// A future remotely published UI can discover choices and defaults without a native binary update.
#[derive(Clone,Debug,Default,PartialEq,Deserialize,Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCatalog { pub enabled:bool,pub defaults:GenerationOptions,pub models:Vec<ModelOption> }
