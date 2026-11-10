//! mySSH 应用后端。M0：壳 + 日志/panic；M1：终端会话（term_* 命令族）。
//! 命令契约见 docs/design/03-ipc-contract.md（逐里程碑补齐）。

mod connect;
mod diagnostics;
mod encoding;
mod exec;
mod files;
mod fs_limiter;
mod governor;
mod lifecycle;
mod local_pty;
mod logging;
mod mcp;
mod mcp_clients;
mod mcp_files;
mod mcp_jobs;
mod mcp_policy;
mod mcp_recipes;
mod mcp_terminal;
mod mcp_text;
mod mcp_ui;
mod monitor;
mod perf;
mod recording;
mod retention;
mod sessions;
mod settings;
mod sftp;
mod stream_protocol;
mod terminal;
mod transfer_ledger;
mod tunnels;
mod wire;

use std::sync::Arc;

use sessions::SessionManagerState;
use terminal::TerminalManager;
use tracing::info;
use tunnels::TunnelManagerState;

#[tauri::command]
async fn log_frontend(msg: String) -> Result<(), String> {
    info!(target: "frontend", "{msg}");
    Ok(())
}

#[tauri::command]
async fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn run() {
    let _log_guard = logging::init();
    info!("mySSH starting");

    // 会话存储打不开属环境级故障：fail loud（M2 起会话/隧道/审计全依赖它）
    let store = tauri::async_runtime::block_on(core_store::Store::open(&sessions::store_path()))
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "会话存储打开失败，终止");
            eprintln!("会话存储打开失败: {e}");
            std::process::exit(1);
        });
    let session_state = Arc::new(SessionManagerState {
        store: Arc::new(store),
        events: lifecycle::SessionEvents::new(),
    });
    let tunnel_mgr_state = Arc::new(TunnelManagerState {
        mgr: core_tunnel::TunnelManager::new(),
        last: parking_lot::Mutex::new(std::collections::HashMap::new()),
    });
    let sftp_state = sftp::SftpManagerState::new(session_state.store.clone());
    let exec_state = exec::ExecManagerState::new(sftp_state.rt());
    let monitor_state = monitor::MonitorState::new();
    let mcp_manager = mcp::McpManager::new();
    let mcp_terms = mcp_terminal::McpTerminalState::new();
    let mcp_jobs = mcp_jobs::McpJobs::new();
    let mcp_ui_hub = mcp_ui::McpUiHub::new();
    let mcp_clients = mcp_clients::McpClients::new();
    let term_mgr = Arc::new(TerminalManager::default());
    let ai_backups = mcp_files::BackupStore::new(mcp_files::BackupStore::default_root());
    let recordings = recording::RecordingStore::new(
        recording::RecordingStore::default_root(),
        Some(session_state.store.clone()),
    );
    // 会话变更失效扇出：命令只发事件，消费方在此登记（隧道/ctx 池）
    lifecycle::wire(
        &session_state.events,
        session_state.store.clone(),
        tunnel_mgr_state.mgr.clone(),
        sftp_state.clone(),
        exec_state.clone(),
    );

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(term_mgr.clone())
        .manage(session_state.clone())
        .manage(tunnel_mgr_state.clone())
        .manage(sftp_state.clone())
        .manage(exec_state.clone())
        .manage(monitor_state)
        .manage(mcp_manager.clone())
        .manage(mcp_terms.clone())
        .manage(mcp_jobs.clone())
        .manage(mcp_ui_hub.clone())
        .manage(mcp_clients.clone())
        .manage(ai_backups.clone())
        .manage(recordings.clone())
        .setup(move |app| {
            // 开机自启隧道：store 已就绪，后台拉起（失败仅日志，监督器自持重连）
            let mgr = tunnel_mgr_state.mgr.clone();
            let store = session_state.store.clone();
            tauri::async_runtime::spawn(async move {
                crate::tunnels::autostart_tunnels(mgr, store).await;
            });
            // 保留策略：AI 改动备份与审计日志按设置天数定期清理，防无限膨胀
            crate::retention::spawn(session_state.store.clone(), ai_backups.clone());
            // MCP 服务端：按 mcp.enabled/port/token 设置启动（绑定失败仅日志）
            let deps = crate::mcp::McpDeps {
                store: session_state.store.clone(),
                sftp: sftp_state.clone(),
                exec: exec_state.clone(),
                terms: mcp_terms.clone(),
                ui: mcp_ui_hub.clone(),
                term_mgr: term_mgr.clone(),
                backups: ai_backups.clone(),
                jobs: mcp_jobs.clone(),
                recs: recordings.clone(),
                clients: mcp_clients.clone(),
            };
            tauri::async_runtime::spawn(async move {
                crate::mcp::boot_from_settings(mcp_manager, deps).await;
            });
            let _ = app;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            log_frontend,
            app_version,
            perf::perf_stats,
            diagnostics::export_diagnostics,
            terminal::term_open,
            terminal::term_input,
            terminal::term_input_raw,
            terminal::term_credit,
            terminal::term_focus,
            terminal::term_resize,
            terminal::term_close,
            terminal::term_record_start,
            terminal::term_record_stop,
            recording::recordings_list,
            recording::recording_load,
            recording::recording_export_text,
            recording::recording_delete,
            recording::recording_verify,
            recording::audit_verify_chain,
            terminal::hostkey_confirm,
            terminal::ki_respond,
            files::read_private_key,
            files::open_local,
            files::local_mkdir,
            files::local_stat,
            files::local_touch,
            files::local_rename,
            files::local_delete,
            files::open_in_explorer,
            files::local_copy,
            files::local_desktop_path,
            files::transfer_save_file,
            sessions::session_list,
            sessions::session_upsert,
            sessions::session_delete,
            sessions::group_rename,
            sessions::group_delete,
            sessions::session_move,
            sessions::cred_set,
            sessions::cred_delete,
            sessions::vault_status,
            sessions::session_test_connect,
            sessions::config_export,
            sessions::config_import,
            tunnels::tunnel_start,
            tunnels::tunnel_stop,
            tunnels::tunnel_list,
            tunnels::tunnel_subscribe,
            tunnels::tunnel_save,
            tunnels::tunnel_check_port,
            tunnels::tunnel_delete,
            tunnels::tunnel_defs,
            sftp::sftp_list,
            sftp::sftp_stat,
            sftp::sftp_mkdir,
            sftp::sftp_delete,
            sftp::sftp_rename,
            sftp::sftp_chmod,
            sftp::sftp_touch,
            sftp::sftp_home,
            sftp::shell_integration_status,
            sftp::shell_integration_set,
            sftp::local_list,
            sftp::sftp_upload,
            sftp::sftp_download,
            sftp::transfer_list,
            sftp::transfer_pause,
            sftp::transfer_resume,
            sftp::transfer_cancel,
            sftp::transfer_retry,
            sftp::transfer_remove,
            sftp::transfer_prioritize,
            sftp::transfer_clear,
            sftp::transfer_pause_all,
            sftp::transfer_resume_all,
            sftp::transfer_subscribe,
            sftp::transfer_job_list,
            sftp::transfer_job_pause,
            sftp::transfer_job_resume,
            sftp::transfer_job_cancel,
            sftp::transfer_job_retry,
            sftp::transfer_job_remove,
            sftp::transfer_history,
            sftp::transfer_history_clear,
            sftp::sftp_edit_open,
            monitor::metrics_subscribe,
            monitor::metrics_unsubscribe,
            settings::settings_list,
            settings::settings_set,
            settings::settings_delete,
            settings::policy_defaults,
            mcp::mcp_status,
            mcp::mcp_restart,
            mcp_ui::mcp_ui_subscribe,
            mcp_ui::mcp_approve,
            mcp_ui::mcp_forget_always,
            mcp_ui::mcp_activity,
            mcp_ui::mcp_activity_stop,
            mcp_ui::mcp_agent_tab_ready,
            mcp_clients::mcp_client_idle_answer,
            mcp_terminal::mcp_share_tab,
            mcp_terminal::mcp_unshare_tab,
            mcp_files::ai_backups_list,
            mcp_files::ai_backup_restore,
            retention::retention_apply,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            tracing::error!(?e, "tauri exited");
            std::process::exit(1);
        });
}
