//! The windows of other applications, for the `!` prefix.
//!
//! Two macOS APIs do the work:
//!
//! * `CGWindowListCopyWindowInfo` lists on-screen windows with their owner and
//!   title. Titles are redacted by macOS unless the process has been granted
//!   Screen Recording, so an untitled list is possible and the caller has to
//!   cope with it.
//! * the Accessibility API (`AXUIElement`) can raise one *specific* window,
//!   which needs the Accessibility permission. Without it the best we can do is
//!   activate the application, which brings forward whichever of its windows
//!   macOS picks.

use crate::apps::AppEntry;
use crate::search::{SearchContext, search_apps};
use objc::class;
use objc::msg_send;
use objc::runtime::Object;
use objc::{sel, sel_impl};
use std::ffi::{CStr, c_char, c_void};

/// `CGWindowListOption` values. The C constants are an enum, so they are not
/// exported symbols and have to be spelled out.
const ON_SCREEN_ONLY: u32 = 1 << 0;
const EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *mut Object;
    static kCGWindowOwnerName: *const Object;
    static kCGWindowName: *const Object;
    static kCGWindowOwnerPID: *const Object;
    static kCGWindowNumber: *const Object;
    static kCGWindowLayer: *const Object;
}

// The accessibility constants live in HIServices, inside ApplicationServices,
// which AppKit does not re-export: without this the link fails on
// `kAXWindowsAttribute` / `kAXTitleAttribute`.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> *mut c_void;
    fn AXUIElementCopyAttributeValue(
        element: *const c_void,
        attribute: *const Object,
        value: *mut *mut c_void,
    ) -> i32;
    fn AXUIElementPerformAction(element: *const c_void, action: *const Object) -> i32;
    fn AXUIElementSetAttributeValue(
        element: *const c_void,
        attribute: *const Object,
        value: *const c_void,
    ) -> i32;
}

/// AX attribute, action and option names.
///
/// The SDK exports these as C globals inside HIServices (part of
/// ApplicationServices), which AppKit does not re-export, so the linker cannot
/// find them. They are only strings — the accessibility API accepts any
/// CFString — so they are built here instead of linked.
const AX_WINDOWS: &str = "AXWindows";
const AX_TITLE: &str = "AXTitle";
const AX_MAIN: &str = "AXMain";
const AX_FRONTMOST: &str = "AXFrontmost";
const AX_RAISE: &str = "AXRaise";
const AX_TRUSTED_PROMPT: &str = "AXTrustedCheckOptionPrompt";

/// An owned `NSString` for one of those names; the caller releases it.
unsafe fn ax_name(text: &str) -> *mut Object {
    unsafe {
        let string: *mut Object = msg_send![class!(NSString), alloc];
        msg_send![string, initWithBytes: text.as_ptr() length: text.len() encoding: 4usize]
    }
}

/// `kCFBooleanTrue` as an object.
unsafe fn boolean_true() -> *mut Object {
    unsafe { msg_send![class!(NSNumber), numberWithBool: true] }
}

/// One ordinary on-screen window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// Owning application's name, e.g. "终端".
    pub app: String,
    /// Window title; empty when macOS withholds it or the window has none.
    pub title: String,
    pub pid: i32,
    /// `CGWindowID`, for logs and as a stable identity within one snapshot.
    pub number: u32,
}

impl WindowInfo {
    /// What to show in the list: the title, prefixed by the application when
    /// the title does not already say which app it is.
    pub fn label(&self) -> String {
        if self.title.is_empty() {
            return self.app.clone();
        }
        if self.app.is_empty()
            || self.title == self.app
            || self.title.starts_with(&format!("{} ", self.app))
        {
            return self.title.clone();
        }
        format!("{} · {}", self.app, self.title)
    }
}

/// Windows read through the accessibility API, cached for a moment: it costs a
/// round trip per application, and a `!` query runs on every keystroke.
static ACCESSIBILITY_WINDOWS: std::sync::Mutex<Option<(std::time::Instant, Vec<WindowInfo>)>> =
    std::sync::Mutex::new(None);

const ACCESSIBILITY_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(2);

/// The windows to offer, and whether macOS withheld every title.
///
/// `CGWindowList` is tried first: no permission, and it reports the real
/// front-to-back order. macOS redacts the titles without the Screen Recording
/// permission, so when they all come back empty the accessibility API is asked
/// instead (it knows the titles, and is also what raises a specific window).
/// Only when neither can produce a title does the list degrade to one row per
/// Dock application — with the flag set, so the caller can say why.
pub fn list_windows() -> (Vec<WindowInfo>, bool) {
    // `CGWindowList` first: no permission needed and it reports the real
    // front-to-back order. It hands out titles only with the Screen Recording
    // permission, which is why the accessibility list is fetched when every
    // title came back empty — note that an empty list can also mean "all
    // windows are minimized", so the accessibility list is worth asking for
    // even then.
    let raw = raw_windows();
    let accessible = if raw.iter().all(|window| window.title.is_empty()) && accessibility_trusted()
    {
        Some(list_via_accessibility())
    } else {
        None
    };
    choose_windows(raw, accessible, is_regular_application)
}

/// Decide what the `!` prefix offers.
///
/// Three-way degradation, and the middle step is the one that is easy to get
/// wrong: a titled `CGWindowList` wins, an accessibility list is used when macOS
/// withheld the titles, and only when neither produced a title does the list
/// become one row per application (with `true`, so the caller can explain).
fn choose_windows(
    raw: Vec<WindowInfo>,
    accessible: Option<Vec<WindowInfo>>,
    is_regular: impl Fn(i32) -> bool,
) -> (Vec<WindowInfo>, bool) {
    let titled: Vec<WindowInfo> = raw
        .iter()
        .filter(|window| !window.title.is_empty())
        .cloned()
        .collect();
    if !titled.is_empty() {
        return (titled, false);
    }
    if let Some(accessible) = accessible
        && !accessible.is_empty()
    {
        return (accessible, false);
    }
    if raw.is_empty() {
        return (Vec::new(), false);
    }

    // One row per application *name*: two instances of the same application are
    // one entry in a switcher, and the front-most one (first here) wins.
    let mut seen = std::collections::HashSet::new();
    let fallback = raw
        .into_iter()
        .filter(|window| is_regular(window.pid))
        .filter(|window| seen.insert(window.app.to_lowercase()))
        .map(|mut window| {
            window.title.clear();
            window
        })
        .collect();
    (fallback, true)
}

/// Windows of every Dock application, through the accessibility API.
fn list_via_accessibility() -> Vec<WindowInfo> {
    if let Ok(cache) = ACCESSIBILITY_WINDOWS.lock()
        && let Some((stamp, windows)) = cache.as_ref()
        && stamp.elapsed() < ACCESSIBILITY_CACHE_TTL
    {
        return windows.clone();
    }

    let frontmost = frontmost_pid();
    let mut windows = Vec::new();
    unsafe {
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        let apps: *mut Object = msg_send![workspace, runningApplications];
        if apps.is_null() {
            return windows;
        }
        let our_pid = std::process::id() as i32;
        let count: usize = msg_send![apps, count];
        let mut targets: Vec<(i32, String)> = Vec::new();
        for index in 0..count {
            let app: *mut Object = msg_send![apps, objectAtIndex: index];
            if app.is_null() {
                continue;
            }
            let policy: i64 = msg_send![app, activationPolicy];
            if policy != 0 {
                continue; // not a Dock application
            }
            let pid: i32 = msg_send![app, processIdentifier];
            if pid <= 0 || pid == our_pid {
                continue;
            }
            let name: *mut Object = msg_send![app, localizedName];
            targets.push((pid, cf_string(name)));
        }
        // The window the user is looking at first, then the rest by name.
        targets.sort_by(|a, b| {
            (a.0 != frontmost)
                .cmp(&(b.0 != frontmost))
                .then_with(|| a.1.cmp(&b.1))
        });
        for (pid, app) in targets {
            windows.extend(windows_of_application(pid, &app));
        }
    }

    if let Ok(mut cache) = ACCESSIBILITY_WINDOWS.lock() {
        *cache = Some((std::time::Instant::now(), windows.clone()));
    }
    windows
}

/// The titled windows of one application, through the accessibility API.
fn windows_of_application(pid: i32, app: &str) -> Vec<WindowInfo> {
    unsafe {
        let element = AXUIElementCreateApplication(pid);
        if element.is_null() {
            return Vec::new();
        }
        let mut value: *mut c_void = std::ptr::null_mut();
        let windows_attribute = ax_name(AX_WINDOWS);
        let status = AXUIElementCopyAttributeValue(
            element,
            windows_attribute,
            &mut value as *mut *mut c_void,
        );
        let _: () = msg_send![windows_attribute, release];
        if status != 0 || value.is_null() {
            return Vec::new();
        }

        let array = value as *mut Object;
        let count: usize = msg_send![array, count];
        let title_attribute = ax_name(AX_TITLE);
        let mut windows = Vec::new();
        for index in 0..count {
            let window: *mut Object = msg_send![array, objectAtIndex: index];
            if window.is_null() {
                continue;
            }
            let mut title: *mut c_void = std::ptr::null_mut();
            let status = AXUIElementCopyAttributeValue(
                window as *const c_void,
                title_attribute,
                &mut title as *mut *mut c_void,
            );
            if status != 0 || title.is_null() {
                continue;
            }
            let text = cf_string(title as *mut Object);
            let _: () = msg_send![title as *mut Object, release];
            if text.is_empty() {
                continue;
            }
            windows.push(WindowInfo {
                app: app.to_string(),
                title: text,
                pid,
                number: index as u32,
            });
        }
        let _: () = msg_send![array, release];
        let _: () = msg_send![title_attribute, release];
        windows
    }
}

/// Process id of the frontmost application.
fn frontmost_pid() -> i32 {
    unsafe {
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: *mut Object = msg_send![workspace, frontmostApplication];
        if app.is_null() {
            return 0;
        }
        msg_send![app, processIdentifier]
    }
}

/// Whether a process is a Dock application (`NSApplicationActivationPolicyRegular`),
/// as opposed to a background service or a menu bar extra. Used to keep macOS
/// helpers out of the title-less fallback list.
fn is_regular_application(pid: i32) -> bool {
    unsafe {
        let app: *mut Object = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        if app.is_null() {
            return false;
        }
        let policy: i64 = msg_send![app, activationPolicy];
        policy == 0
    }
}

/// Every on-screen window of other applications, front to back, with the
/// titles macOS is willing to give us (possibly all empty).
///
/// Untitled windows are *kept* here: whether they exist is how the caller tells
/// "macOS withheld the titles" apart from "nothing is open", and the degraded
/// per-application list is built from them.
fn raw_windows() -> Vec<WindowInfo> {
    let mut windows = Vec::new();
    unsafe {
        let array = CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP_ELEMENTS, 0);
        if array.is_null() {
            return windows;
        }
        let count: usize = msg_send![array, count];
        let our_pid = std::process::id() as i32;
        for index in 0..count {
            let info: *mut Object = msg_send![array, objectAtIndex: index];
            if info.is_null() {
                continue;
            }
            if number(info, kCGWindowLayer) != 0 {
                continue;
            }
            let pid = number(info, kCGWindowOwnerPID) as i32;
            if pid <= 0 || pid == our_pid {
                continue;
            }
            let title = string(info, kCGWindowName);
            let app = string(info, kCGWindowOwnerName);
            if app.is_empty() {
                continue;
            }
            windows.push(WindowInfo {
                app,
                title,
                pid,
                number: number(info, kCGWindowNumber) as u32,
            });
        }
        let _: () = msg_send![array, release];
    }
    windows
}

/// Whether this process may use the accessibility API.
pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Ask macOS to show the accessibility prompt for this application.
pub fn request_accessibility() {
    unsafe {
        let key = ax_name(AX_TRUSTED_PROMPT);
        if key.is_null() {
            return;
        }
        let options: *mut Object =
            msg_send![class!(NSDictionary), dictionaryWithObject: boolean_true() forKey: key];
        AXIsProcessTrustedWithOptions(options as *const c_void);
        let _: () = msg_send![key, release];
    }
}

/// Bring a window to the front.
///
/// Always activates the owning application; then raises the exact window when
/// the accessibility permission allows it. Returns whether the exact window
/// could be raised.
pub fn focus(window: &WindowInfo) -> bool {
    crate::activate_app(window.pid);
    if !accessibility_trusted() {
        return false;
    }
    unsafe {
        let app = AXUIElementCreateApplication(window.pid);
        if app.is_null() {
            return false;
        }
        let mut windows: *mut c_void = std::ptr::null_mut();
        let windows_attribute = ax_name(AX_WINDOWS);
        let status =
            AXUIElementCopyAttributeValue(app, windows_attribute, &mut windows as *mut *mut c_void);
        let _: () = msg_send![windows_attribute, release];
        if status != 0 || windows.is_null() {
            return false;
        }

        let array = windows as *mut Object;
        let count: usize = msg_send![array, count];
        let title_attribute = ax_name(AX_TITLE);
        let main_attribute = ax_name(AX_MAIN);
        let frontmost_attribute = ax_name(AX_FRONTMOST);
        let raise_action = ax_name(AX_RAISE);

        let mut raised = false;
        for index in 0..count {
            let element: *mut Object = msg_send![array, objectAtIndex: index];
            if element.is_null() {
                continue;
            }
            let mut title: *mut c_void = std::ptr::null_mut();
            AXUIElementCopyAttributeValue(
                element as *const c_void,
                title_attribute,
                &mut title as *mut *mut c_void,
            );
            let same = cf_string(title as *mut Object) == window.title;
            if !title.is_null() {
                let _: () = msg_send![title as *mut Object, release];
            }
            if !same {
                continue;
            }
            // Make it the main window first: raising a background window is a
            // no-op.
            AXUIElementSetAttributeValue(
                element as *const c_void,
                main_attribute,
                boolean_true() as *const c_void,
            );
            AXUIElementPerformAction(element as *const c_void, raise_action);
            raised = true;
            break;
        }
        AXUIElementSetAttributeValue(app, frontmost_attribute, boolean_true() as *const c_void);
        let _: () = msg_send![array, release];
        for name in [
            title_attribute,
            main_attribute,
            frontmost_attribute,
            raise_action,
        ] {
            let _: () = msg_send![name, release];
        }
        raised
    }
}

/// Rank the windows matching `query`. An empty query keeps the front-to-back
/// order macOS reports.
pub fn search(query: &str, windows: &[WindowInfo], ctx: &SearchContext) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..windows.len()).collect();
    }
    // Reuse the application scorer: the title becomes the display name and the
    // application name is indexed like a bundle name, so a title match outranks
    // an application match.
    let entries: Vec<AppEntry> = windows
        .iter()
        .map(|window| {
            AppEntry::with_display_name(
                window.app.clone(),
                format!("window:{}:{}", window.pid, window.number),
                Some(window.title.clone()),
            )
        })
        .collect();
    search_apps(query, &entries, ctx)
        .into_iter()
        .map(|(index, _)| index)
        .collect()
}

/// Read an `NSNumber` (a toll-free `CFNumber`) out of a window dictionary.
unsafe fn number(info: *mut Object, key: *const Object) -> i64 {
    unsafe {
        let value: *mut Object = msg_send![info, objectForKey: key];
        if value.is_null() {
            return 0;
        }
        msg_send![value, longLongValue]
    }
}

/// Read an `NSString` (a toll-free `CFString`) out of a window dictionary.
unsafe fn string(info: *mut Object, key: *const Object) -> String {
    unsafe {
        let value: *mut Object = msg_send![info, objectForKey: key];
        cf_string(value)
    }
}

/// Convert a toll-free `CFString` to a Rust string; `null` becomes empty.
unsafe fn cf_string(value: *mut Object) -> String {
    unsafe {
        if value.is_null() {
            return String::new();
        }
        let utf8: *const c_char = msg_send![value, UTF8String];
        if utf8.is_null() {
            return String::new();
        }
        CStr::from_ptr(utf8).to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchTuning;
    use crate::usage::UsageStore;

    fn window(app: &str, title: &str) -> WindowInfo {
        WindowInfo {
            app: app.to_string(),
            title: title.to_string(),
            pid: 1,
            number: 1,
        }
    }

    fn context<'a>(usage: &'a UsageStore, tuning: &'a SearchTuning) -> SearchContext<'a> {
        SearchContext {
            apps_only: false,
            usage,
            tuning,
            now: 0,
        }
    }

    #[test]
    fn titles_are_ranked_above_application_names() {
        let windows = [
            window("Safari", "Apple"),
            window("Notes", "Safari tips"),
            window("Safari", "Docs"),
        ];
        let usage = UsageStore::in_memory();
        let tuning = SearchTuning::default();
        let hits = search("safari", &windows, &context(&usage, &tuning));
        assert_eq!(hits.len(), 3, "everything containing the word is listed");
        assert_eq!(
            hits[0], 1,
            "a window whose title matches outranks one whose app does"
        );
        assert_eq!(
            &hits[1..],
            &[0, 2],
            "the two application-name matches follow, by title"
        );
    }

    #[test]
    fn an_empty_query_keeps_the_front_to_back_order() {
        let windows = [
            window("A", "first"),
            window("B", "second"),
            window("C", "third"),
        ];
        let usage = UsageStore::in_memory();
        let tuning = SearchTuning::default();
        assert_eq!(
            search("", &windows, &context(&usage, &tuning)),
            vec![0, 1, 2]
        );
        assert_eq!(
            search("   ", &windows, &context(&usage, &tuning)),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn unrelated_queries_return_nothing() {
        let windows = [window("Safari", "Docs")];
        let usage = UsageStore::in_memory();
        let tuning = SearchTuning::default();
        assert!(search("zzzz", &windows, &context(&usage, &tuning)).is_empty());
    }

    #[test]
    fn labels_do_not_repeat_the_application_name() {
        assert_eq!(window("QQ", "QQ").label(), "QQ");
        assert_eq!(window("Safari", "Docs").label(), "Safari · Docs");
        assert_eq!(window("Safari", "").label(), "Safari");
        assert_eq!(window("Safari", "Safari tips").label(), "Safari tips");
    }

    /// macOS withheld every title: `CGWindowList` still knows the windows, so
    /// the accessibility list is the next source.
    #[test]
    fn hidden_titles_fall_back_to_the_accessibility_list() {
        let raw = [window("Safari", ""), window("Notes", "")];
        let accessible = [window("Safari", "Docs"), window("Notes", "Todo")];
        let (windows, withheld) = choose_windows(raw.to_vec(), Some(accessible.to_vec()), |_| true);
        assert_eq!(windows, accessible.to_vec());
        assert!(!withheld, "titles were obtained after all");
    }

    /// Neither source could produce a title: one row per application, and the
    /// flag that makes the caller explain why.
    #[test]
    fn without_any_permission_the_list_is_one_row_per_application() {
        let untitled = |app: &str, pid: i32| WindowInfo {
            app: app.to_string(),
            title: String::new(),
            pid,
            number: pid as u32,
        };
        let raw = [
            untitled("Safari", 1),
            untitled("Safari", 9), // a second instance of the same application
            untitled("Notes", 2),
            untitled("SomeHelper", 3),
        ];
        // 1 and 2 are Dock applications, 3 is a helper.
        let (windows, withheld) = choose_windows(raw.to_vec(), None, |pid| pid != 3);
        assert!(withheld);
        assert_eq!(windows.len(), 2, "one row per application: {:?}", windows);
        assert!(windows.iter().all(|window| window.title.is_empty()));
        assert_eq!(windows[0].app, "Safari");
        assert_eq!(windows[1].app, "Notes");
    }

    /// Nothing is open at all — not the same thing as hidden titles, and it must
    /// not produce the permission notice.
    #[test]
    fn an_empty_window_list_is_not_a_permission_problem() {
        let (windows, withheld) = choose_windows(Vec::new(), None, |_| true);
        assert!(windows.is_empty());
        assert!(!withheld, "no windows is not a missing permission");
    }

    /// Titles present: they win, and the accessibility list is not even asked
    /// for (the caller passes `None`).
    #[test]
    fn titles_from_the_window_list_win() {
        let raw = [window("Safari", "Docs"), window("Notes", "Todo")];
        let (windows, withheld) = choose_windows(raw.to_vec(), None, |_| true);
        assert_eq!(windows, raw.to_vec());
        assert!(!withheld);
    }

    /// The list has to be usable on this machine: either real titles, or the
    /// per-application fallback with the "titles withheld" flag set.
    #[test]
    fn the_window_list_is_usable() {
        let (windows, withheld) = list_windows();
        eprintln!(
            "windows: {} (titles withheld: {withheld}) -> {:?}",
            windows.len(),
            windows.iter().map(|w| w.label()).collect::<Vec<_>>()
        );
        if withheld {
            assert!(
                windows.iter().all(|window| window.title.is_empty()),
                "the fallback lists applications, not windows"
            );
            let mut pids: Vec<i32> = windows.iter().map(|window| window.pid).collect();
            pids.sort_unstable();
            let unique = pids.len();
            pids.dedup();
            assert_eq!(pids.len(), unique, "one row per application");
        } else {
            assert!(
                windows.iter().all(|window| !window.title.is_empty()),
                "titled listings never contain blank rows"
            );
        }
    }
}
