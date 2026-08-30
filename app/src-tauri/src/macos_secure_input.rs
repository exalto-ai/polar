//! Native provider-key entry. Secrets never become webview values.

use std::sync::mpsc;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSSecureTextField};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use zeroize::Zeroizing;

async fn on_main_thread<T: Send + 'static>(
    app: tauri::AppHandle,
    task: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (send, receive) = mpsc::sync_channel(1);
    app.run_on_main_thread(move || {
        let _ = send.send(task());
    })
    .map_err(|_| "Could not open the native provider dialog.".to_string())?;
    tauri::async_runtime::spawn_blocking(move || receive.recv())
        .await
        .map_err(|_| "The native provider dialog stopped unexpectedly.".to_string())?
        .map_err(|_| "The native provider dialog closed unexpectedly.".to_string())?
}

pub async fn prompt_key(
    app: tauri::AppHandle,
    provider: &'static str,
    replacing: bool,
) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    on_main_thread(app, move || {
        let marker = MainThreadMarker::new()
            .ok_or_else(|| "The provider dialog must run on the main thread.".to_string())?;
        let alert = NSAlert::new(marker);
        alert.setMessageText(&NSString::from_str(&format!(
            "{} your {provider} API key",
            if replacing { "Replace" } else { "Add" }
        )));
        alert.setInformativeText(&NSString::from_str(
            "The key stays in your Mac login Keychain. Adding it sends no document text. Provider API usage may cost money when you use built-in chat.",
        ));
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

        if alert.runModal() != NSAlertFirstButtonReturn {
            field.setStringValue(&NSString::from_str(""));
            return Ok(None);
        }
        let value = Zeroizing::new(field.stringValue().to_string());
        field.setStringValue(&NSString::from_str(""));
        Ok(Some(Zeroizing::new(value.as_bytes().to_vec())))
    })
    .await
}

pub async fn confirm_remove(app: tauri::AppHandle, provider: &'static str) -> Result<bool, String> {
    on_main_thread(app, move || {
        let marker = MainThreadMarker::new()
            .ok_or_else(|| "The provider dialog must run on the main thread.".to_string())?;
        let alert = NSAlert::new(marker);
        alert.setMessageText(&NSString::from_str(&format!("Remove {provider} key?")));
        alert.setInformativeText(&NSString::from_str(
            "This removes Proof of Thought’s local Keychain copy. It does not revoke the key at the provider.",
        ));
        alert.addButtonWithTitle(&NSString::from_str("Remove"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        Ok(alert.runModal() == NSAlertFirstButtonReturn)
    })
    .await
}
