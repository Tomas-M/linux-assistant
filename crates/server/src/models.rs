use std::collections::HashSet;
use protocol::GenerationOptions;
use crate::config::{Config,FieldError};

/// A disabled gate preserves today's behavior; later enable it and publish selector UI, without rebuilding.
pub fn resolve(config:&Config,requested:&GenerationOptions)->Result<GenerationOptions,FieldError> {
    requested.validate().map_err(|error|FieldError::new("generation",&error.to_string()))?;
    let defaults=GenerationOptions { model:config.model.clone(),reasoning_effort:config.reasoning_effort.clone() };
    if !config.allow_model_selection { return Ok(defaults); }
    let model=requested.model.as_ref().or(config.model.as_ref()).ok_or_else(||FieldError::new("generation.model","Choose a model before selecting its reasoning level"))?;
    let available=config.available_models.iter().find(|available|&available.id==model).ok_or_else(||FieldError::new("generation.model","This model is not available for client selection"))?;
    let global_effort=if Some(model)==config.model.as_ref() { config.reasoning_effort.as_ref() } else { None };
    let effort=requested.reasoning_effort.as_ref().or(global_effort).or(available.reasoning_levels.first());
    if effort.is_some_and(|effort|!available.reasoning_levels.contains(effort)) { return Err(FieldError::new("generation.reasoning_effort","This reasoning level is not available for the selected model")); }
    // Explicit defaults prevent a loaded provider thread from inheriting a previous task's choice.
    Ok(GenerationOptions { model:Some(model.clone()),reasoning_effort:effort.cloned() })
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
    if config.allow_model_selection {
        let default=config.available_models.iter().find(|model|Some(&model.id)==config.model.as_ref()).ok_or_else(||FieldError::new("model","Before enabling client selection, set a default model included in Available models"))?;
        if config.reasoning_effort.as_ref().is_some_and(|effort|!default.reasoning_levels.contains(effort)) { return Err(FieldError::new("reasoning_effort","The default reasoning level must be allowed for the default model")); }
    }
    Ok(())
}
