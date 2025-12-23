use base64::{engine::general_purpose::STANDARD, Engine};
use std::fs;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

/// Read an image file and return it as a base64 data URL
#[tauri::command]
fn read_image_as_base64(path: String) -> Result<String, String> {
    // Read the file
    let data = fs::read(&path).map_err(|e| format!("Failed to read file: {}", e))?;

    // Determine MIME type from extension
    let mime_type = if path.to_lowercase().ends_with(".png") {
        "image/png"
    } else if path.to_lowercase().ends_with(".jpg") || path.to_lowercase().ends_with(".jpeg") {
        "image/jpeg"
    } else if path.to_lowercase().ends_with(".gif") {
        "image/gif"
    } else if path.to_lowercase().ends_with(".webp") {
        "image/webp"
    } else {
        "application/octet-stream"
    };

    // Encode to base64 and return as data URL
    let base64_data = STANDARD.encode(&data);
    Ok(format!("data:{};base64,{}", mime_type, base64_data))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![greet, read_image_as_base64])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
