pub mod bilibili;
pub mod commands;
pub mod compat;
pub mod components;
pub mod convert_queue;
pub mod easytier;
pub mod estimate;
pub mod error;
pub mod external_player;
pub mod http_server;
pub mod logger;
pub mod media_library;
pub mod network_interfaces;
pub mod settings;
pub mod system_codecs;
pub mod thumbnail;
pub mod transcode;
pub mod url_builder;

use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex, RwLock};

use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::ManagerExt;

use commands::AppState;
use http_server::HttpServer;
use settings::CloseBehavior;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 阶段 3.2：单实例保护，防僵尸进程
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tracing::info!("尝试启动第二个实例");
            // 显示已有实例的主窗口
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .setup(|app| {
            let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let paths = settings::resolve_paths(&exe);
            let _ = std::fs::create_dir_all(&paths.thumb_dir);
            // 清理上次转码/下载中断遗留的临时产物与占位空文件
            {
                let converted = paths.data_dir.join("converted");
                let downloads = paths.download_dir.clone();
                
                // converted 目录：.part.mp4 临时产物 + 0 字节占位空文件
                if converted.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(&converted) {
                        for e in entries.flatten() {
                            let path = e.path();
                            let name = e.file_name().to_string_lossy().to_string();
                            if name.ends_with(".part.mp4") {
                                let _ = std::fs::remove_file(&path);
                            } else if name.ends_with(".mp4") {
                                // 删除 unique_dest 预占的 0 字节空文件
                                if let Ok(meta) = e.metadata() {
                                    if meta.len() == 0 {
                                        let _ = std::fs::remove_file(&path);
                                    }
                                }
                            }
                        }
                    }
                }
                
                // 下载目录：.part 临时文件
                if downloads.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(&downloads) {
                        for e in entries.flatten() {
                            if e.file_name().to_string_lossy().ends_with(".part") {
                                let _ = std::fs::remove_file(e.path());
                            }
                        }
                    }
                }
            }
            let recent_logs = logger::init(&paths.log_dir);
            tracing::info!(
                "应用启动，数据目录: {} (便携模式: {})",
                paths.data_dir.display(),
                paths.portable
            );

            // 后台跟踪 VRChat 客户端日志，把 AVPro 解码路径/播放错误同步到本应用日志
            logger::spawn_vrchat_log_tailer();

            let app_settings = settings::Settings::load(&paths.settings_file)
                .unwrap_or_else(|e| {
                    tracing::warn!("读取设置失败，使用默认设置: {}", e);
                    settings::Settings::default()
                });

            // 窗口收到托盘后进程会被 Windows 视为后台，可能触发电源节流（EcoQoS）降权，
            // 导致 HTTP 供数断续（VR 播放时 CPU 被 VRChat 占满，降权进程更易被饿死）。
            // 媒体服务进程申请豁免；系统不支持或失败仅记日志，不影响启动。
            // 设置项「禁用电源节流」默认开启，可在设置页关闭。
            #[cfg(windows)]
            if app_settings.disable_power_throttling {
                use windows::Win32::System::Threading::{
                    GetCurrentProcess, SetProcessInformation, ProcessPowerThrottling,
                    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
                    PROCESS_POWER_THROTTLING_STATE,
                };
                let throttle = PROCESS_POWER_THROTTLING_STATE {
                    Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                    ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
                    StateMask: 0,
                };
                let r = unsafe {
                    SetProcessInformation(
                        GetCurrentProcess(),
                        ProcessPowerThrottling,
                        &throttle as *const _ as *const _,
                        std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
                    )
                };
                match r {
                    Ok(()) => tracing::info!("已申请进程电源节流豁免（EcoQoS opt-out）"),
                    Err(e) => tracing::warn!("申请电源节流豁免失败（忽略）: {}", e),
                }
            }

            // 初始化开机启动状态（以系统实际状态为准修正配置）
            let autolaunch = app.handle().autolaunch();
            let mut settings_fixed = app_settings.clone();
            if let Ok(enabled) = autolaunch.is_enabled() {
                if enabled != app_settings.autostart {
                    settings_fixed.autostart = enabled;
                }
            }

            let db = media_library::Db::open(&paths.db_file).map_err(|e| {
                tracing::error!("数据库初始化失败: {}", e);
                e
            })?;
            let db = Arc::new(Mutex::new(db));

            let thumb_dir = if settings_fixed.thumb_dir.trim().is_empty() {
                paths.thumb_dir.clone()
            } else {
                std::path::PathBuf::from(settings_fixed.thumb_dir.trim())
            };
            let hls_dir = paths.data_dir.join("hls");
            let allow_peer_view = Arc::new(AtomicBool::new(settings_fixed.allow_peer_view));
            let stream_buf_kb = Arc::new(AtomicUsize::new(settings_fixed.stream_buffer_kb as usize));
            let server = HttpServer::new(
                db.clone(),
                &thumb_dir,
                &hls_dir,
                allow_peer_view.clone(),
                stream_buf_kb.clone(),
            );

            let state = AppState {
                paths: paths.clone(),
                settings: RwLock::new(settings_fixed.clone()),
                db,
                server: server.clone(),
                recent_logs,
                allow_peer_view,
                stream_buf_kb,
                easytier_forced_lan: AtomicBool::new(false),
                easytier: easytier::EasytierManager::new(paths.data_dir.clone()),
                convert_queue: convert_queue::ConvertQueue::default(),
                hls_jobs: RwLock::new(std::collections::HashSet::new()),
            };
            app.manage(state);
            app.manage(estimate::EstimateState::default());

            // 转码队列执行器
            commands::spawn_convert_worker(app.handle().clone());

            // 自动启动 HTTP 服务
            let handle = app.handle().clone();
            let settings_for_start = settings_fixed.clone();
            tauri::async_runtime::spawn(async move {
                let server = {
                    let st = handle.state::<AppState>();
                    st.server.clone()
                };
                if let Err(e) = server
                    .start(&settings_for_start.bind_ip(), settings_for_start.port)
                    .await
                {
                    tracing::error!("HTTP 服务自动启动失败: {}", e);
                }
                let _ = handle.emit("server-status-changed", server.status());
            });

            // 系统托盘
            build_tray(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                let behavior = state.current_settings().close_behavior;
                if behavior == CloseBehavior::Tray {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    // 退出前停止 HTTP 服务与组网进程（组网提权运行，退出时不阻塞等待）
                    state.server.stop();
                    state.easytier.stop_detached();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::reset_settings,
            commands::get_server_status,
            commands::start_server,
            commands::stop_server,
            commands::restart_server,
            commands::list_media,
            commands::import_files,
            commands::import_online_video,
            commands::refresh_media,
            commands::remove_media,
            commands::set_custom_thumbnail,
            commands::get_network_interfaces,
            commands::build_media_url,
            commands::detect_players,
            commands::open_in_player,
            commands::open_with_system_default,
            commands::open_in_folder,
            commands::open_url,
            commands::system_audio_decoder_missing,
            commands::get_recent_logs,
            commands::get_app_paths,
            commands::is_first_launch,
            commands::mark_disclaimer_accepted,
            commands::open_data_dir,
            commands::open_dir,
            commands::clear_thumbnail_cache,
            commands::get_convert_cache_info,
            commands::open_convert_dir,
            commands::clear_convert_cache,
            commands::get_close_behavior,
            commands::get_components_status,
            commands::download_component,
            commands::open_component_page,
            commands::get_easytier_status,
            commands::start_easytier,
            commands::stop_easytier,
            commands::get_room_info,
            commands::test_peer_connection,
            commands::read_easytier_log,
            commands::reset_network_adapter,
            commands::fetch_peer_media,
            commands::download_peer_media_task,
            commands::download_task_pause,
            commands::download_task_resume,
            commands::get_media_streams,
            estimate::load_estimate_source,
            estimate::probe_transcode_estimate,
            estimate::cancel_transcode_estimate,
            commands::transcode_media,
            commands::transcode_output_exists,
            commands::generate_sample,
            commands::reveal_path,
            commands::segment_transcode,
            commands::list_segments,
            commands::list_convert_tasks,
            commands::convert_task_start,
            commands::convert_task_cancel,
            commands::convert_task_remove,
            commands::convert_task_clear,
            // HLS 边转边播已停用（容易出现 Live 进度条/音画不同步等问题），改为分段转码
            // commands::make_hls,
            // commands::get_hls_status,
            // commands::build_hls_url,
            commands::list_folders,
            commands::create_folder,
            commands::rename_folder,
            commands::delete_folder,
            commands::rename_media,
            commands::move_media,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn build_tray(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let show = MenuItemBuilder::with_id("show", "显示主窗口").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "退出").build(app)?;
    let menu = MenuBuilder::new(app).items(&[&show, &quit]).build()?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .tooltip("VRchat-LocalCinema")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            "quit" => {
                let state = app.state::<AppState>();
                state.server.stop();
                state.easytier.stop_detached();
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}
