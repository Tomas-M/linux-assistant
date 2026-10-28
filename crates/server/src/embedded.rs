use anyhow::Result;
use codex_app_server::{AppServerRuntimeOptions, AppServerTransport, PluginStartupTasks, RemoteControlStartupMode};
use codex_utils_cli::CliConfigOverrides;
use codex_config::LoaderOverrides;

/// Link the upstream server into this executable; each account runs in its own child process.
pub async fn run(paths: codex_arg0::Arg0DispatchPaths) -> Result<()> {
    let mut overrides = CliConfigOverrides::default();
    for flag in ["shell_tool","unified_exec","apps","plugins","multi_agent","multi_agent_v2","memories",
        "computer_use","browser_use","browser_use_external","in_app_browser","image_generation","view_image",
        "code_mode","code_mode_host","code_mode_only","request_permissions_tool","shell_snapshot","skill_search"] {
        overrides.raw_overrides.push(format!("features.{flag}=false"));
    }
    overrides.raw_overrides.extend(["cli_auth_credentials_store=\"file\"","web_search=\"disabled\"",
        "skills.bundled.enabled=false","skills.include_instructions=false","project_doc_max_bytes=0"].map(str::to_owned));
    let loader = LoaderOverrides { ignore_user_config:true,ignore_project_config:true,ignore_user_and_project_exec_policy_rules:true,..Default::default() };
    codex_app_server::run_main_with_transport_options(paths,overrides,loader,false,false,
        AppServerTransport::Stdio,Default::default(),Default::default(),AppServerRuntimeOptions {
            plugin_startup_tasks:PluginStartupTasks::Skip,
            remote_control_startup_mode:RemoteControlStartupMode::DisabledEphemeral,
            ..Default::default()
        }).await?;
    Ok(())
}
