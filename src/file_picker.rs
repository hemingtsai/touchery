//! Native macOS file picker.
//!
//! GPUI has no file dialog, so this is a thin `NSOpenPanel` wrapper. The panel
//! runs its own modal loop (`runModal`), which keeps the whole dialog —
//! navigation, sidebar, keyboard — native; the price is that the control panel
//! does not repaint until the user answers, exactly like any other modal
//! dialog on macOS.

use std::path::{Path, PathBuf};

/// Ask the user to choose a file with the given extension.
///
/// Returns the chosen path, or `None` when the dialog was cancelled or could
/// not be created.
pub fn pick_file(
    message: &str,
    prompt: &str,
    extension: &str,
    start_dir: Option<&Path>,
) -> Option<PathBuf> {
    use objc::class;
    use objc::msg_send;
    use objc::rc::autoreleasepool;
    use objc::runtime::Object;
    use objc::sel;
    use objc::sel_impl;
    use std::ffi::{CStr, CString};

    let message = CString::new(message).ok()?;
    let prompt = CString::new(prompt).ok()?;
    let extension = CString::new(extension).ok()?;
    let start_dir = match start_dir {
        Some(dir) => Some(CString::new(dir.to_string_lossy().as_bytes()).ok()?),
        None => None,
    };

    autoreleasepool(|| unsafe {
        // The control panel is a normal window and the user just clicked in it,
        // so the app is already active; activating again costs nothing and
        // makes sure the panel comes up in front of it.
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, activateIgnoringOtherApps: true];

        let panel: *mut Object = msg_send![class!(NSOpenPanel), openPanel];
        if panel.is_null() {
            return None;
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

        // NSModalResponseOK
        let response: isize = msg_send![panel, runModal];
        if response != 1 {
            return None;
        }

        let url: *mut Object = msg_send![panel, URL];
        if url.is_null() {
            return None;
        }
        let path: *mut Object = msg_send![url, path];
        if path.is_null() {
            return None;
        }
        let utf8: *const std::ffi::c_char = msg_send![path, UTF8String];
        if utf8.is_null() {
            return None;
        }
        Some(PathBuf::from(
            CStr::from_ptr(utf8).to_string_lossy().into_owned(),
        ))
    })
}
