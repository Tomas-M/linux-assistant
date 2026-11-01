use anyhow::Result;
use codex_app_server::{AppServerRuntimeOptions, AppServerTransport, PluginStartupTasks, RemoteControlStartupMode};
use codex_utils_cli::CliConfigOverrides;
use codex_config::LoaderOverrides;

/// Link the upstream server into this executable; each account runs in its own child process.
pub async fn run(paths: codex_arg0::Arg0DispatchPaths) -> Result<()> {
    let mut overrides = CliConfigOverrides::default();
    for flag in ["shell_tool","unified_exec","apps","plugins","multi_agent","multi_agent_v2","memories",
        "computer_use","browser_use","browser_use_external","in_app_browser","image_generation","view_image",
        "code_mode","code_mode_only","request_permissions_tool","shell_snapshot","skill_search"] {
        overrides.raw_overrides.push(format!("features.{flag}=false"));
    }
    // Backend sandbox instructions must not mislabel client-executed tools as read-only.
    overrides.raw_overrides.extend(["include_permissions_instructions=false","features.code_mode_host=true","cli_auth_credentials_store=\"file\"","web_search=\"live\"","features.default_mode_request_user_input=true","tools.experimental_request_user_input.enabled=true","tools.update_plan.enabled=true",
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

/// An installer-created helper symlink lets every worker run the original executable directly.
pub fn installed_helper(executable:&std::path::Path)->bool {
    let helper=executable.with_file_name(if cfg!(windows) { "codex-code-mode-host.exe" } else { "codex-code-mode-host" });
    match (helper.canonicalize(),executable.canonicalize()) { (Ok(helper),Ok(executable))=>helper==executable,_=>false }
}

/// One source-linked executable supplies both App Server and its sandboxed JavaScript tool orchestrator.
pub fn runtime(executable:&std::path::Path,home:&std::path::Path)->Result<(tempfile::TempDir,std::path::PathBuf)> {
    let directory=tempfile::Builder::new().prefix("runtime-").tempdir_in(home)?;
    let worker=directory.path().join(if cfg!(windows) { "assistant-server.exe" } else { "assistant-server" });
    if std::fs::hard_link(executable,&worker).is_err() { std::fs::copy(executable,&worker)?; }
    std::fs::hard_link(&worker,directory.path().join(if cfg!(windows) { "codex-code-mode-host.exe" } else { "codex-code-mode-host" }))?;
    Ok((directory,worker))
}
