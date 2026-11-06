use std::collections::HashSet;
use protocol::GenerationOptions;
use crate::config::{Config,FieldError};

/// A disabled gate preserves today's behavior; later enable it and publish selector UI, without rebuilding.
pub fn resolve(config:&Config,requested:&GenerationOptions)->Result<GenerationOptions,FieldError> {
    requested.validate().map_err(|error|FieldError::new("generation",&error.to_string()))?;
    let defaults=GenerationOptions { model:config.model.clone(),reasoning_effort:config.reasoning_effort.clone() };
    if !config.allow_model_selection || requested==&GenerationOptions::default() { return Ok(defaults); }
    let model=requested.model.as_ref().or(config.model.as_ref()).ok_or_else(||FieldError::new("generation.model","Choose a model before selecting its reasoning level"))?;
    let available=config.available_models.iter().find(|available|&available.id==model).ok_or_else(||FieldError::new("generation.model","This model is not available for client selection"))?;
    if let Some(effort)=&requested.reasoning_effort {
        if !available.reasoning_levels.contains(effort) { return Err(FieldError::new("generation.reasoning_effort","This reasoning level is not available for the selected model")); }
    }
    // Selecting another model without an effort uses that provider model's default, not a mismatched global effort.
    Ok(GenerationOptions { model:Some(model.clone()),reasoning_effort:requested.reasoning_effort.clone() })
}

/// The catalog is an operator-managed allowlist; identifiers stay data rather than compiled enum variants.
pub fn validate(config:&Config)->Result<(),FieldError> {
    let mut models=HashSet::new();
    for (index,model) in config.available_models.iter().enumerate() {
        let field=format!("available_models.{index}");
        GenerationOptions { model:Some(model.id.clone()),reasoning_effort:None }.validate().map_err(|error|FieldError::new(&format!("{field}.id"),&error.to_string()))?;
        if !models.insert(&model.id) { return Err(FieldError::new(&format!("{field}.id"),"Each model must have a different identifier")); }
        let mut levels=HashSet::new();
        for level in &model.reasoning_levels {
            GenerationOptions { model:None,reasoning_effort:Some(level.clone()) }.validate().map_err(|error|FieldError::new(&format!("{field}.reasoning_levels"),&error.to_string()))?;
            if !levels.insert(level) { return Err(FieldError::new(&format!("{field}.reasoning_levels"),"Each reasoning level must appear only once per model")); }
        }
    }
    if config.allow_model_selection && config.available_models.is_empty() { return Err(FieldError::new("available_models","Add at least one model before enabling client selection")); }
    Ok(())
}
