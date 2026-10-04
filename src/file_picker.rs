//! Native macOS file picker, presented asynchronously.
//!
//! GPUI has no file dialog, so this is a thin `NSOpenPanel` wrapper — but the
//! panel must never be run modally (`runModal`) from a gpui handler: that opens
//! a nested run loop while the handler is still on the stack, AppKit keeps
//! delivering events to gpui, and the update cycle re-enters itself
//! (`RefCell already borrowed`, then `failed to initiate panic … aborting` —
//! the app disappears).
//!
//! Instead the panel is presented with `beginWithCompletionHandler:`, which
//! returns immediately. The completion block only writes into a plain
//! `Arc<Mutex<…>>`: it must not touch gpui at all, because it runs on the main
//! thread from inside AppKit's event delivery, where the app may already be
//! borrowed. The control panel's refresh timer reads the slot and finishes the
//! job from a normal gpui callback.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// How the user answered the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickOutcome {
    Picked(PathBuf),
    Cancelled,
}

/// Slot the completion block writes and the UI polls.
pub type PickSlot = Arc<Mutex<Option<PickOutcome>>>;

pub fn new_slot() -> PickSlot {
    Arc::new(Mutex::new(None))
}

/// Take the answer, if the user has given one since the last call.
pub fn take_outcome(slot: &PickSlot) -> Option<PickOutcome> {
    slot.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Present an open panel filtered to `extension`. Returns as soon as the panel
/// is on screen; the answer lands in `slot`.
pub fn pick_file(
    message: &str,
    prompt: &str,
    extension: &str,
    start_dir: Option<&Path>,
    slot: PickSlot,
) {
    use block::ConcreteBlock;
    use objc::class;
    use objc::msg_send;
    use objc::rc::autoreleasepool;
    use objc::runtime::Object;
    use objc::sel;
    use objc::sel_impl;
    use std::ffi::{CStr, CString};

    let Ok(message) = CString::new(message) else {
        return;
    };
    let Ok(prompt) = CString::new(prompt) else {
        return;
    };
    let Ok(extension) = CString::new(extension) else {
        return;
    };
    let start_dir = start_dir.and_then(|dir| CString::new(dir.to_string_lossy().as_bytes()).ok());

    autoreleasepool(|| unsafe {
        // An accessory app is not necessarily active when the button is
        // clicked; the panel has to come up in front of the control panel.
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, activateIgnoringOtherApps: true];

        let panel: *mut Object = msg_send![class!(NSOpenPanel), openPanel];
        if panel.is_null() {
            return;
        }

        let message: *mut Object =
            msg_send![class!(NSString), stringWithUTF8String: message.as_ptr()];
        let prompt: *mut Object =
            msg_send![class!(NSString), stringWithUTF8String: prompt.as_ptr()];
        let extension: *mut Object =
            msg_send![class!(NSString), stringWithUTF8String: extension.as_ptr()];
        let extensions: *mut Object = msg_send![class!(NSArray), arrayWithObject: extension];

        let _: () = msg_send![panel, setMessage: message];
        let _: () = msg_send![panel, setPrompt: prompt];
        let _: () = msg_send![panel, setCanChooseFiles: true];
        let _: () = msg_send![panel, setCanChooseDirectories: false];
        let _: () = msg_send![panel, setAllowsMultipleSelection: false];
        let _: () = msg_send![panel, setResolvesAliases: true];
        // Deprecated in favour of content types on 12+, still honoured; the
        // caller validates the extension as well.
        let _: () = msg_send![panel, setAllowedFileTypes: extensions];

        if let Some(dir) = start_dir {
            let path: *mut Object = msg_send![class!(NSString), stringWithUTF8String: dir.as_ptr()];
            let url: *mut Object = msg_send![class!(NSURL), fileURLWithPath: path];
            let _: () = msg_send![panel, setDirectoryURL: url];
        }

        // The closure body is lexically inside the `unsafe` block above; it
        // runs later, from AppKit's event delivery, but the obligations are the
        // same. Everything in it is plain memory: no gpui.
        let block = ConcreteBlock::new(move |response: isize| {
            // NSModalResponseOK
            let outcome = if response == 1 {
                let path: *const std::ffi::c_char = {
                    let url: *mut Object = msg_send![panel, URL];
                    if url.is_null() {
                        std::ptr::null()
                    } else {
                        let path: *mut Object = msg_send![url, path];
                        if path.is_null() {
                            std::ptr::null()
                        } else {
                            msg_send![path, UTF8String]
                        }
                    }
                };
                if path.is_null() {
                    PickOutcome::Cancelled
                } else {
                    PickOutcome::Picked(PathBuf::from(
                        CStr::from_ptr(path).to_string_lossy().into_owned(),
                    ))
                }
            } else {
                PickOutcome::Cancelled
            };
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
        });
        // AppKit copies the handler, so the local block can go away.
        let block = block.copy();
        let _: () = msg_send![panel, beginWithCompletionHandler: &*block];
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slot_hands_the_answer_over_once() {
        let slot = new_slot();
        assert_eq!(take_outcome(&slot), None, "nothing picked yet");

        *slot.lock().unwrap() = Some(PickOutcome::Picked(PathBuf::from("/tmp/calc.lua")));
        assert_eq!(
            take_outcome(&slot),
            Some(PickOutcome::Picked(PathBuf::from("/tmp/calc.lua")))
        );
        assert_eq!(take_outcome(&slot), None, "an answer is only consumed once");

        *slot.lock().unwrap() = Some(PickOutcome::Cancelled);
        assert_eq!(take_outcome(&slot), Some(PickOutcome::Cancelled));
    }
}
