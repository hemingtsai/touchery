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

use gpui::{App, Bounds, DisplayId, Pixels, PlatformDisplay, Size, point, px};
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
/// See [`fit_centered`] for the placement rules; this just feeds it the
/// display's size.
pub fn centered_bounds(
    display: &dyn PlatformDisplay,
    size: Size<Pixels>,
    top_ratio: Option<f64>,
) -> Bounds<Pixels> {
    fit_centered(display.bounds().size, size, top_ratio)
}

/// Centre `size` within `available`, shrinking and clamping so the result
/// always fits inside `available`.
///
/// `top_ratio` puts the window's top edge that fraction of the way down the
/// display (Spotlight-style); `None` centres it vertically. Both are clamped so
/// a display too small for the requested placement degrades to the closest
/// fully-visible position rather than hanging off an edge.
///
/// Note that `display.bounds()` is only trustworthy for its *size* — see the
/// module docs for why the origin is ignored.
fn fit_centered(
    available: Size<Pixels>,
    size: Size<Pixels>,
    top_ratio: Option<f64>,
) -> Bounds<Pixels> {
    let (available_w, available_h) = (available.width.to_f64(), available.height.to_f64());
    let (w, h) = (
        size.width.to_f64().min(available_w),
        size.height.to_f64().min(available_h),
    );

    let x = (available_w - w) / 2.0;
    // The top edge cannot sit lower than this without pushing the window's
    // bottom past the display's bottom edge.
    let max_y = (available_h - h).max(0.0);
    let y = match top_ratio {
        Some(ratio) => (available_h * ratio).clamp(0.0, max_y),
        None => max_y / 2.0,
    };

    Bounds::new(
        point(px(x as f32), px(y as f32)),
        gpui::size(px(w as f32), px(h as f32)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds_of(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), gpui::size(px(w), px(h)))
    }

    /// Every result must satisfy `0 <= origin` and `origin + size <= available`,
    /// since gpui offsets the window by the display's own origin.
    fn assert_fits(available: Bounds<Pixels>, result: Bounds<Pixels>) {
        assert!(result.origin.x >= px(0.), "x origin negative: {result:?}");
        assert!(result.origin.y >= px(0.), "y origin negative: {result:?}");
        assert!(
            result.origin.x + result.size.width <= available.size.width,
            "overflows right: {result:?} in {available:?}"
        );
        assert!(
            result.origin.y + result.size.height <= available.size.height,
            "overflows bottom: {result:?} in {available:?}"
        );
    }

    #[test]
    fn spotlight_centers_horizontally_and_honors_top_ratio() {
        let display = bounds_of(0., 0., 3440., 1440.);
        let result = fit_centered(display.size, gpui::size(px(680.), px(440.)), Some(0.30));
        assert_eq!(result, bounds_of(1380., 432., 680., 440.));
    }

    #[test]
    fn center_ignores_top_ratio() {
        let display = bounds_of(0., 0., 3440., 1440.);
        let result = fit_centered(display.size, gpui::size(px(560.), px(520.)), None);
        assert_eq!(result, bounds_of(1440., 460., 560., 520.));
    }

    #[test]
    fn clamps_top_ratio_on_a_short_display() {
        // 500px tall, 440px window: 30% would be y=150, which would push the
        // bottom to 590 — past the display. Clamp to the max, y=60.
        let display = bounds_of(0., 0., 1280., 500.);
        let result = fit_centered(display.size, gpui::size(px(680.), px(440.)), Some(0.30));
        assert_eq!(result, bounds_of(300., 60., 680., 440.));
        assert_fits(display, result);
    }

    #[test]
    fn shrinks_to_fit_a_narrower_display() {
        let display = bounds_of(0., 0., 480., 800.);
        let result = fit_centered(display.size, gpui::size(px(680.), px(440.)), Some(0.30));
        assert_eq!(result.size, gpui::size(px(480.), px(440.)));
        assert_eq!(result.origin.x, px(0.));
        assert_fits(display, result);
    }

    #[test]
    fn clamps_when_display_is_shorter_than_the_window() {
        let display = bounds_of(0., 0., 1024., 300.);
        let result = fit_centered(display.size, gpui::size(px(680.), px(440.)), Some(0.30));
        assert_eq!(result.size.height, px(300.));
        assert_eq!(result.origin.y, px(0.));
        assert_fits(display, result);
    }

    #[test]
    fn exact_fit_pins_to_the_top() {
        // The window fills the display vertically, so the top ratio has no
        // room to move it: y clamps to 0 rather than overflowing the bottom.
        let display = bounds_of(0., 0., 680., 440.);
        let result = fit_centered(display.size, gpui::size(px(680.), px(440.)), Some(0.30));
        assert_eq!(result, bounds_of(0., 0., 680., 440.));
        assert_fits(display, result);
    }
}

