// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod adapters;
mod commands;
mod domain;
mod services;
#[cfg(test)]
mod test_utils;

fn main() {
    let mut builder = tauri::Builder::default();
    // Single-instance — ПЕРВЫМ: второй запуск (kraimaps:// при открытом окне)
    // гасится здесь, фокусирует существующее окно. С фичей "deep-link" сам URL
    // уже переслан в первый экземпляр через deep-link событие (onOpenUrl/getCurrent
    // на фронте), вручную парсить argv не нужно.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            use tauri::Manager;
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.unminimize();
                let _ = win.show();
                let _ = win.set_focus();
            }
        }));
    }
    builder
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        // NB: здесь НЕТ register_all() для dev-режима — осознанно.
        // register_all() пишет handler kraimaps:// в HKCU на текущий exe,
        // запись переживает выход из `tauri dev` и затеняет УСТАНОВЛЕННОЕ
        // приложение: сайт будит закрытый установщик через debug-exe,
        // а тот грузит devUrl (localhost:1420) без запущенного vite → ERR.
        // Тест deep-link в dev: передать URL аргументом —
        // `.\target\debug\installer.exe "kraimaps://install/1"`
        // (плагин читает argv через getCurrent, см. App.vue).
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::get_installer_version,
            commands::analyze_pack,
            commands::detect_launchers,
            commands::resolve_loader_version,
            commands::create_instance,
            commands::ensure_tlauncher_version,
            commands::prepare_instance_icon,
            commands::icon_data_url,
            commands::resource_pack_icon,
            commands::deploy_pack,
            commands::close_launcher,
            commands::reopen_launcher,
            commands::suggest_world_name,
            commands::launch_game,
            commands::oauth_start,
            commands::oauth_exchange,
            commands::auth_profile,
            commands::auth_logout,
            commands::auth_store_status,
            commands::get_entitlement,
            commands::get_manifest,
            commands::download_map_file,
            commands::install_start,
            commands::install_complete,
            commands::discard_temp_files,
            commands::updater_check,
            commands::updater_install,
        ])
        .setup(|_app| {
            // Стартовый проход по временному мусору — фоном, без ожидания:
            // страховка от крашей/убийств процесса, метёт только наше
            // старше 6 часов (см. `temp_clean`). Старт не ждёт и не падает.
            std::thread::spawn(crate::services::temp_clean::sweep_stale_temp);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Map Installer");
}