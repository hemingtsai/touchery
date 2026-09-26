//! Screen lookup and window placement geometry.
//!
//! `cx.displays()` cannot answer "which display is the cursor on" on macOS:
//! gpui's `MacDisplay::bounds()` discards `CGDisplayBounds.origin` and reports
//! only the size, so every display comes back rooted at (0, 0). Hit-testing
//! those bounds therefore just picks whichever display happens to come first in
//! `CGGetActiveDisplayList` order whose *size* box contains the point — which
//! says nothing about the display the cursor is actually on.
//!
//! Origin information comes from AppKit's `NSScreen.frame` instead. It lives in
//! the same coordinate space as `NSEvent.mouseLocation`: origin at the
//! bottom-left of the primary display, y pointing up. No flipping required.
//!
//! Placement bounds stay *display-local*: gpui resolves `WindowBounds` against
//! the target screen's own frame (it adds `screen_frame.origin` when creating
//! the `NSWindow`), so the origin produced here is relative to the display it
//! will be shown on.

use gpui::{App, Bounds, DisplayId, Pixels, PlatformDisplay, Size, point};
use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};
use std::rc::Rc;

/// A rectangle in AppKit's global screen space (y pointing up).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct ScreenRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl ScreenRect {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// Global mouse position in AppKit coordinates (origin at the bottom-left of
/// the primary screen, y up).
#[repr(C)]
#[derive(Clone, Copy)]
struct NSPoint {
    x: f64,
    y: f64,
}

unsafe impl objc::Encode for NSPoint {
    fn encode() -> objc::Encoding {
        // Objective-C type encoding for CGPoint { double x; double y; }.
        unsafe { objc::Encoding::from_str("{CGPoint=dd}") }
    }
}

fn mouse_location() -> Option<(f64, f64)> {
    unsafe {
        // +[NSEvent mouseLocation] returns an NSPoint by value (never NULL);
        // the only failure mode is a missing NSEvent class, which cannot
        // happen on any macOS version this app supports. We still guard
        // against a null class pointer for soundness.
        let ns_event: *mut Object = msg_send![class!(NSEvent), class];
        if ns_event.is_null() {
            return None;
        }
        let point: NSPoint = msg_send![ns_event, mouseLocation];
        Some((point.x, point.y))
    }
}

/// Every active screen, paired with the raw `CGDirectDisplayID` AppKit reports
/// as `NSScreenNumber`.
///
/// That value is the same id space gpui builds `DisplayId` from (see
/// `MacDisplay::all` / `MacDisplay::primary`), so the two can be matched
/// directly without going through `CGDisplayCreateUUIDFromDisplayID`.
fn ns_screens() -> Vec<(ScreenRect, u32)> {
    let mut screens = Vec::new();
    unsafe {
        let ns_screens: *mut Object = msg_send![class!(NSScreen), screens];
        if ns_screens.is_null() {
            return screens;
        }
        let key: *mut Object =
            msg_send![class!(NSString), stringWithUTF8String: c"NSScreenNumber".as_ptr()];
        if key.is_null() {
            return screens;
        }
        let count: usize = msg_send![ns_screens, count];
        for index in 0..count {
            let screen: *mut Object = msg_send![ns_screens, objectAtIndex: index];
            if screen.is_null() {
                continue;
            }
            let description: *mut Object = msg_send![screen, deviceDescription];
            if description.is_null() {
                continue;
            }
            let number: *mut Object = msg_send![description, objectForKey: key];
            if number.is_null() {
                continue;
            }
            let frame: ScreenRect = msg_send![screen, frame];
            let display_id: u32 = msg_send![number, unsignedIntegerValue];
            screens.push((frame, display_id));
        }
    }
    screens
}

/// The display currently under the mouse cursor.
///
/// `None` means the cursor is not over any display — the menu bar, the Dock, or
/// a gap in a non-rectangular arrangement. Callers should fall back to the
/// primary display rather than treating that as an error.
pub fn display_under_cursor(cx: &App) -> Option<DisplayId> {
    let (x, y) = mouse_location()?;
    let (_, raw_id) = ns_screens()
        .into_iter()
        .find(|(frame, _)| frame.contains(x, y))?;
    cx.displays()
        .into_iter()
        .find(|display| u32::from(display.id()) == raw_id)
        .map(|display| display.id())
}

/// The display a new window should appear on: the one under the mouse cursor,
/// falling back to the primary display.
pub fn target_display(cx: &App) -> Option<Rc<dyn PlatformDisplay>> {
    display_under_cursor(cx)
        .and_then(|id| cx.find_display(id))
        .or_else(|| cx.primary_display())
}

/// Bounds for a `size` window centred horizontally on `display`.
///
/// `top_ratio` puts the window's top edge that fraction of the way down the
/// display (Spotlight-style); `None` centres it vertically instead.
///
/// Note that `display.bounds()` is only trustworthy for its *size* — see the
/// module docs for why the origin is ignored.
pub fn centered_bounds(
    display: &dyn PlatformDisplay,
    size: Size<Pixels>,
    top_ratio: Option<f64>,
) -> Bounds<Pixels> {
    let available = display.bounds().size;
    let y = match top_ratio {
        Some(ratio) => available.height * ratio as f32,
        None => (available.height - size.height) / 2.0,
    };
    let x = (available.width - size.width) / 2.0;
    Bounds::new(point(x, y), size)
}

