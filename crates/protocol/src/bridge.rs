use std::collections::BTreeMap;
use serde::{Deserialize,Serialize};

pub const BRIDGE_API:u32=1;
pub const MAX_UI_BYTES:usize=8*1024*1024;
#[derive(Clone,Debug,Deserialize,Serialize)]
pub struct Bridge { pub api:u32,pub tools:Vec<String> }
impl Bridge {
    /// Advertise only operations implemented by this installed native client.
    pub fn current()->Self { Self { api:BRIDGE_API,tools:crate::tool_specs().iter().map(|tool|tool["name"].as_str().unwrap().into()).collect() } }
}
#[derive(Deserialize,Serialize)]
pub struct UiBundle { pub bridge_api:u32,pub version:String,pub files:BTreeMap<String,UiAsset> }
#[derive(Deserialize,Serialize)]
pub struct UiAsset { pub content_type:String,pub content:String }
