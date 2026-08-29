//! Bridge AppKit termination requests into the document close guard.
//!
//! Tao 0.35 installs an application delegate without
//! `applicationShouldTerminate:`. AppKit therefore proceeds directly from a
//! Dock, logout, or Apple-event quit to `applicationWillTerminate:`, bypassing
//! Tauri's cancellable window-close events. Adding the optional delegate
//! method lets AppKit wait while webviews autosave or ask whether to export.
//! This bridge is pinned to Tao 0.35's delegate behavior. Re-audit it whenever
//! Tauri or Tao changes, especially if Tao gains its own termination callback.

use std::error::Error;
use std::sync::OnceLock;

use dispatch2::DispatchQueue;
use objc2::runtime::{AnyObject, ClassBuilder, ProtocolObject, Sel};
use objc2::{MainThreadMarker, sel};
use objc2_app_kit::{NSApplication, NSApplicationDelegate, NSApplicationTerminateReply};
use tauri::Manager;

static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

/// Install the optional AppKit delegate callback on Tao's existing delegate.
///
/// A no-ivar subclass preserves Tao's delegate layout and inherited launch,
/// reopen, URL, and shutdown callbacks without mutating Tao's registered class
/// for every delegate instance in the process.
pub fn install(handle: tauri::AppHandle) -> Result<(), Box<dyn Error>> {
    let marker = MainThreadMarker::new().ok_or_else(|| {
        std::io::Error::other("termination guard must install on the main thread")
    })?;
    let application = NSApplication::sharedApplication(marker);
    let delegate = application
        .delegate()
        .ok_or_else(|| std::io::Error::other("macOS application delegate is missing"))?;
    let delegate_protocol: &ProtocolObject<dyn NSApplicationDelegate> = &delegate;
    let delegate_object: &AnyObject = delegate_protocol.as_ref();
    let superclass = delegate_object.class();
    let selector = sel!(applicationShouldTerminate:);

    if superclass.responds_to(selector) {
        eprintln!(
            "macOS application delegate already handles termination; skipping the Tao 0.35 bridge and requiring a termination-guard audit"
        );
        return Ok(());
    }

    APP_HANDLE
        .set(handle)
        .map_err(|_| std::io::Error::other("macOS termination guard was installed twice"))?;

    let mut subclass = ClassBuilder::new(c"ThoughtTerminationGuardDelegate", superclass)
        .ok_or_else(|| {
            std::io::Error::other("macOS termination delegate subclass already exists")
        })?;
    unsafe {
        subclass.add_method(
            selector,
            application_should_terminate
                as extern "C" fn(
                    *mut AnyObject,
                    Sel,
                    *mut NSApplication,
                ) -> NSApplicationTerminateReply,
        );
    }
    let subclass = subclass.register();
    let previous = unsafe { AnyObject::set_class(delegate_object, subclass) };
    debug_assert!(std::ptr::eq(previous, superclass));

    Ok(())
}

/// Resolve an earlier `NSTerminateLater` response on AppKit's next main-queue
/// turn. Deferring avoids re-entering Tao's borrowed event callback when an
/// affirmative reply synchronously advances application termination.
pub fn reply(handle: &tauri::AppHandle, should_terminate: bool) {
    let handle_for_reply = handle.clone();
    DispatchQueue::main().exec_async(move || {
        let Some(marker) = MainThreadMarker::new() else {
            eprintln!("macOS termination reply did not run on the main thread");
            return;
        };
        if let Some(state) = handle_for_reply.try_state::<super::QuitState>() {
            state.native_reply_sent();
        }
        NSApplication::sharedApplication(marker)
            .replyToApplicationShouldTerminate(should_terminate);
    });
}

extern "C" fn application_should_terminate(
    _: *mut AnyObject,
    _: Sel,
    _: *mut NSApplication,
) -> NSApplicationTerminateReply {
    // Never unwind through Objective-C. A failure to consult application state
    // conservatively cancels termination instead of losing document changes.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(handle) = APP_HANDLE.get() else {
            return NSApplicationTerminateReply::TerminateCancel;
        };
        if handle.webview_windows().is_empty() {
            return NSApplicationTerminateReply::TerminateNow;
        }
        if super::request_guarded_quit(handle, true) {
            NSApplicationTerminateReply::TerminateLater
        } else {
            NSApplicationTerminateReply::TerminateCancel
        }
    })) {
        Ok(reply) => reply,
        Err(_) => {
            if let Some(handle) = APP_HANDLE.get()
                && let Some(state) = handle.try_state::<super::QuitState>()
            {
                state.abort();
            }
            NSApplicationTerminateReply::TerminateCancel
        }
    }
}
