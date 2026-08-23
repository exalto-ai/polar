/// The probe reports its verdict over IPC rather than on screen, so results can
/// be read from stdout without screenshotting the user's desktop.
#[tauri::command]
fn report(payload: String) {
    println!("=== SELFTEST BEGIN ===");
    println!("{}", payload);
    println!("=== SELFTEST END ===");
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![report])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
