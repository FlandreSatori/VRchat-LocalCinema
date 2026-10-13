pub mod commands;
pub mod error;
pub mod external_player;
pub mod http_server;
pub mod logger;
pub mod media_library;
pub mod network_interfaces;
pub mod settings;
pub mod thumbnail;
pub mod url_builder;

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
            let recent_logs = logger::init(&paths.log_dir);
            tracing::info!(
                "应用启动，数据目录: {} (便携模式: {})",
                paths.data_dir.display(),
                paths.portable
            );

            let app_settings = settings::Settings::load(&paths.settings_file)
                .unwrap_or_else(|e| {
                    tracing::warn!("读取设置失败，使用默认设置: {}", e);
                    settings::Settings::default()
                });

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
            let server = HttpServer::new(db.clone(), &thumb_dir);

            let state = AppState {
                paths: paths.clone(),
                settings: RwLock::new(settings_fixed.clone()),
                db,
                server: server.clone(),
                recent_logs,
            };
            app.manage(state);

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
                    // 退出前停止 HTTP 服务
                    state.server.stop();
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
            commands::get_recent_logs,
            commands::get_app_paths,
            commands::open_data_dir,
            commands::clear_thumbnail_cache,
            commands::get_close_behavior,
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
