//! Native API-key entry for the Pro provider path.
//!
//! The webview asks only for a fixed provider. The secret is collected by an
//! AppKit secure text field and returned directly to native Rust memory. It is
//! never an HTML input, Tauri command argument, event payload, or JavaScript
//! value.

use std::sync::mpsc;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSSecureTextField};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use zeroize::Zeroizing;

pub async fn prompt(
    app: tauri::AppHandle,
    provider_name: &'static str,
    replacing: bool,
) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    app.run_on_main_thread(move || {
        let result = prompt_on_main_thread(provider_name, replacing);
        let _ = sender.send(result);
    })
    .map_err(|_| "Could not open secure API key entry.".to_string())?;

    tauri::async_runtime::spawn_blocking(move || receiver.recv())
        .await
        .map_err(|_| "Secure API key entry stopped unexpectedly.".to_string())?
        .map_err(|_| "Secure API key entry closed unexpectedly.".to_string())?
}

fn prompt_on_main_thread(
    provider_name: &str,
    replacing: bool,
) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    let marker = MainThreadMarker::new()
        .ok_or_else(|| "Secure API key entry must run on the main thread.".to_string())?;
    let alert = NSAlert::new(marker);
    let action = if replacing { "Replace" } else { "Add" };
    alert.setMessageText(&NSString::from_str(&format!(
        "{action} your {provider_name} API key"
    )));
    alert.setInformativeText(&NSString::from_str(&format!(
        "Paste the key below. {provider_name} bills future API usage to your account. This setup check only reads its model catalog and does not generate AI output. Proof of Thought saves a successful key in your Mac login Keychain. No document text or files are sent during this check."
    )));
    alert.addButtonWithTitle(&NSString::from_str(if replacing {
        "Replace key"
    } else {
        "Save key"
    }));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));

    let field = NSSecureTextField::new(marker);
    field.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(360.0, 24.0),
    ));
    field.setPlaceholderString(Some(&NSString::from_str("Paste API key")));
    alert.setAccessoryView(Some(&field));
    alert.layout();
    let _ = alert.window().makeFirstResponder(Some(&field));

    let response = alert.runModal();
    if response != NSAlertFirstButtonReturn {
        field.setStringValue(&NSString::from_str(""));
        return Ok(None);
    }

    let value = field.stringValue();
    let length = value.len();
    let pointer = value.UTF8String().cast::<u8>();
    if pointer.is_null() && length != 0 {
        field.setStringValue(&NSString::from_str(""));
        return Err("The API key could not be read safely.".to_string());
    }
    let bytes = if length == 0 {
        Vec::new()
    } else {
        // SAFETY: `UTF8String` points to at least `NSString::len()` bytes while
        // `value` is alive. Copy immediately into zeroizing native storage.
        unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec()
    };
    field.setStringValue(&NSString::from_str(""));
    drop(value);
    Ok(Some(Zeroizing::new(bytes)))
}
