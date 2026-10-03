// objc 0.2's class!/msg_send! macros internally check cfg(feature = "cargo-clippy"),
// which cargo cannot know about; silence the resulting false-positive lints.
#![allow(unexpected_cfgs)]
use std::collections::{HashMap, HashSet};
use std::process::Command;

use pinyin::ToPinyinMulti;

/// One searchable segment of an app's location: either a localized ancestor
/// folder ("实用工具") or the app itself (last element).
#[derive(Debug, Clone)]
pub struct PathComponent {
    pub name_lower: String,
    /// All full-pinyin variants (polyphonic chars produce multiple).
    pub pinyins: Vec<String>,
    /// All initial-letter variants (e.g. 音乐 -> ["yl", "yy"]).
    pub initials: Vec<String>,
}

impl PathComponent {
    pub(crate) fn new(name: String) -> Self {
        let name_lower = name.to_lowercase();
        let (pinyins, initials) = pinyin_variants(&name);
        Self {
            name_lower,
            pinyins,
            initials,
        }
    }
}

/// Max number of cartesian-product pinyin variants per string. App names are
/// short; polyphonic characters multiply candidates but stay far below this.
const MAX_PINYIN_VARIANTS: usize = 64;

/// Generate all full-pinyin and initial-letter combinations for `name`,
/// treating every polyphonic character's pronunciations as alternatives.
/// e.g. 音乐 -> (["yinle", "yinyue"], ["yl", "yy"]).
pub(crate) fn pinyin_variants(name: &str) -> (Vec<String>, Vec<String>) {
    fn product(lists: Vec<Vec<String>>) -> Vec<String> {
        let mut acc: Vec<String> = vec![String::new()];
        for list in lists {
            if list.is_empty() {
                continue; // character contributes nothing (non-Han)
            }
            let mut next = Vec::with_capacity(acc.len() * list.len());
            for prefix in &acc {
                for item in &list {
                    next.push(format!("{prefix}{item}"));
                }
            }
            if next.len() > MAX_PINYIN_VARIANTS {
                next.truncate(MAX_PINYIN_VARIANTS);
            }
            acc = next;
        }
        acc
    }

    let mut full_lists: Vec<Vec<String>> = Vec::new();
    let mut initial_lists: Vec<Vec<String>> = Vec::new();

    for multi in name.to_pinyin_multi() {
        let Some(multi) = multi else {
            continue; // non-Han character: dropped, matching old behavior
        };
        let mut fulls = Vec::new();
        let mut inits = Vec::new();
        for py in multi {
            let plain = py.plain();
            fulls.push(plain.to_string());
            if let Some(c) = plain.chars().next() {
                inits.push(c.to_string());
            }
        }
        full_lists.push(fulls);
        initial_lists.push(inits);
    }

    (product(full_lists), product(initial_lists))
}

#[derive(Debug, Clone)]
pub struct AppEntry {
    /// Bundle file stem, e.g. "WeChat" (fallback label + search field).
    pub name: String,
    /// Localized display name per the user's locale, e.g. "微信".
    pub display_name: String,
    pub path: String,
    pub display_name_lower: String,
    pub name_lower: String,
    /// Pinyin variants derived from the localized display name
    /// (polyphonic-aware: 音乐 -> ["yinle", "yinyue"]).
    pub pinyins: Vec<String>,
    pub initials: Vec<String>,
    /// Localized hierarchy from the nearest Applications root down to the
    /// app itself, e.g. ["实用工具", "磁盘工具"]. Empty when the app sits
    /// directly inside a root.
    pub path_components: Vec<PathComponent>,
    /// True when the bundle lives under an Applications root (/Applications,
    /// /System/Applications, ~/Applications, ...) — i.e. a user-facing app.
    pub in_app_dir: bool,
}

impl AppEntry {
    /// Convenience constructor; only used by tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new(name: String, path: String) -> Self {
        Self::with_display_name(name, path, None)
    }

    pub(crate) fn with_display_name(name: String, path: String, localized: Option<String>) -> Self {
        let display_name = localized.unwrap_or_else(|| {
            // Fallback when Spotlight gave us nothing: ask LaunchServices
            // directly (respects locale for bundled processes, may not for
            // unbundled ones — hence it is only a fallback).
            localized_display_name(&path).unwrap_or_else(|| name.clone())
        });
        let display_name_lower = display_name.to_lowercase();
        let name_lower = name.to_lowercase();

        let (pinyins, initials) = pinyin_variants(&display_name);

        let (path_components, in_app_dir) = build_path_components(&path, &display_name);

        Self {
            name,
            display_name,
            path,
            display_name_lower,
            name_lower,
            pinyins,
            initials,
            path_components,
            in_app_dir,
        }
    }
}

const APPS_MARKER: &str = "/Applications/";

/// Roots that hold user-facing apps without containing an `/Applications/`
/// component. Finder and its sibling "Finder applications" (AirDrop, Computer,
/// Recents, Network) live in CoreServices, so they would otherwise be dropped
/// by the "apps only" filter.
const EXTRA_USER_ROOTS: &[&str] = &["/System/Library/CoreServices/"];

thread_local! {
    /// Localized folder names, keyed by absolute path.
    ///
    /// The cache is per thread rather than per enumeration on purpose: most
    /// apps share the same few ancestors (Utilities, …), so a full scan hits
    /// NSFileManager once per distinct folder instead of once per app, and the
    /// names of existing folders do not change while the app runs.
    static FOLDER_NAMES: std::cell::RefCell<HashMap<String, String>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Localized name of a folder, memoized for this thread.
fn localized_folder(folder_path: &str) -> String {
    FOLDER_NAMES.with(|cache| {
        if let Some(hit) = cache.borrow().get(folder_path) {
            return hit.clone();
        }
        let name = localized_display_name(folder_path).unwrap_or_else(|| folder_name(folder_path));
        cache
            .borrow_mut()
            .insert(folder_path.to_string(), name.clone());
        name
    })
}

fn folder_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Walk the folder hierarchy between the nearest Applications root and the
/// app bundle, localizing each level.
fn build_path_components(bundle_path: &str, app_display_name: &str) -> (Vec<PathComponent>, bool) {
    let Some(root_idx) = bundle_path.find(APPS_MARKER) else {
        // Not under an Applications root: treat the app alone as its own
        // component (still slash-searchable). Apps under a known
        // user-facing root (CoreServices: Finder & friends) still count as
        // in-scope; anything else is flagged out-of-scope.
        let in_app_dir = EXTRA_USER_ROOTS
            .iter()
            .any(|root| bundle_path.starts_with(root));
        return (
            vec![PathComponent::new(app_display_name.to_string())],
            in_app_dir,
        );
    };

    let mut components = Vec::new();

    // Everything between "<root>/Applications/" and "<Name>.app" is folders.
    let after_root = &bundle_path[root_idx + APPS_MARKER.len()..];
    let mut walked = String::from(&bundle_path[..root_idx + APPS_MARKER.len() - 1]);
    let segments: Vec<&str> = after_root.split('/').collect();
    for seg in &segments[..segments.len().saturating_sub(1)] {
        if seg.is_empty() {
            continue;
        }
        walked.push('/');
        walked.push_str(seg);
        components.push(PathComponent::new(localized_folder(&walked)));
    }
    components.push(PathComponent::new(app_display_name.to_string()));

    (components, true)
}

/// Ask LaunchServices/NSFileManager for the localized display name of a file
/// or folder (handles macOS `.localized` directories). Used for app names
/// when Spotlight gives nothing and for folder localization.
///
/// Safety: standard NSFileManager selectors with null checks on every
/// returned object; the C string is copied into an owned String before any
/// use, so no borrowed Objective-C memory escapes the call.
fn localized_display_name(path: &str) -> Option<String> {
    use objc::{class, msg_send, runtime::Object, sel, sel_impl};
    use std::ffi::{CStr, CString};

    unsafe {
        let c_path = CString::new(path).ok()?;
        let ns_path: *mut Object =
            msg_send![class!(NSString), stringWithUTF8String: c_path.as_ptr()];
        if ns_path.is_null() {
            return None;
        }
        let manager: *mut Object = msg_send![class!(NSFileManager), defaultManager];
        let display: *mut Object = msg_send![manager, displayNameAtPath: ns_path];
        if display.is_null() {
            return None;
        }
        let utf8: *const std::os::raw::c_char = msg_send![display, UTF8String];
        if utf8.is_null() {
            return None;
        }
        let result = CStr::from_ptr(utf8).to_string_lossy().into_owned();
        Some(result)
    }
}

const QUERY: &str = "kMDItemContentType == 'com.apple.application-bundle'";

/// Add a single app to the index if it's a valid .app bundle.
#[cfg(test)]
pub fn add_app_to_index(path: &str, apps: &mut Vec<AppEntry>) -> bool {
    if let Some(entry) = make_entry(path.to_string(), None) {
        // Check for duplicates by path only (display name may differ for
        // legitimately different apps at different paths).
        let is_dup = apps.iter().any(|e| e.path == entry.path);
        if !is_dup {
            apps.push(entry);
            return true;
        }
    }
    false
}

/// Remove an app from the index by path.
#[cfg(test)]
pub fn remove_app_from_index(path: &str, apps: &mut Vec<AppEntry>) -> bool {
    let len_before = apps.len();
    apps.retain(|e| e.path != path);
    apps.len() < len_before
}

/// Parse one line of `mdfind -attr kMDItemDisplayName` output:
/// `<path>   kMDItemDisplayName = <localized name>`
fn parse_attr_line(line: &str) -> Option<(String, Option<String>)> {
    const MARKER: &str = "kMDItemDisplayName = ";
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    match line.find(MARKER) {
        Some(pos) => {
            let path = line[..pos].trim().to_string();
            let display = line[pos + MARKER.len()..].trim().to_string();
            if path.is_empty() {
                return None;
            }
            Some((path, (!display.is_empty()).then_some(display)))
        }
        None => Some((line.to_string(), None)),
    }
}

fn make_entry(path: String, localized: Option<String>) -> Option<AppEntry> {
    let name = std::path::Path::new(&path)
        .file_stem()?
        .to_str()?
        .to_string();
    if name.starts_with('.') || name.to_lowercase().contains("uninstall") {
        return None;
    }
    Some(AppEntry::with_display_name(name, path, localized))
}

pub fn enumerate_apps() -> Vec<AppEntry> {
    // Primary source: Spotlight returns the localized display name in one
    // shot, honoring the user's locale (e.g. "微信" on zh-Hans systems).
    let output = Command::new("mdfind")
        .args(["-attr", "kMDItemDisplayName", QUERY])
        .output();

    let entries: Vec<AppEntry> = match output {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout
                .lines()
                .filter_map(|line| {
                    let (path, localized) = parse_attr_line(line)?;
                    make_entry(path, localized)
                })
                .collect()
        }
        _ => {
            // Spotlight unavailable/disabled or still empty: walk the known
            // application roots. Repeating the same mdfind query cannot help —
            // it fails for exactly the same reason the first one did.
            eprintln!("[apps] Spotlight returned no applications; scanning the filesystem instead");
            scan_app_roots()
        }
    };

    dedup_entries(entries)
}

/// Roots scanned when Spotlight cannot answer. They cover user-installed apps,
/// system apps, Finder and friends, plus the per-user Applications folder.
const FALLBACK_ROOTS: &[&str] = &[
    "/Applications",
    "/System/Applications",
    "/System/Library/CoreServices",
];

/// How deep below a root to look for bundles. Applications normally sit in a
/// root or one folder down (`Utilities`); the limit also stops symlink cycles.
const FALLBACK_MAX_DEPTH: usize = 3;

/// Enumerate application bundles by walking the known roots.
fn scan_app_roots() -> Vec<AppEntry> {
    let mut roots: Vec<std::path::PathBuf> = FALLBACK_ROOTS
        .iter()
        .map(std::path::PathBuf::from)
        .collect();
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join("Applications"));
    }

    let mut entries = Vec::new();
    for root in roots {
        collect_bundles(&root, 0, &mut entries);
    }
    entries
}

/// Collect `.app` bundles below `dir` into `out`, without descending into a
/// bundle: the applications inside one are helpers, not entries to launch.
fn collect_bundles(dir: &std::path::Path, depth: usize, out: &mut Vec<AppEntry>) {
    if depth > FALLBACK_MAX_DEPTH {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "app") {
            // Never descend into a bundle: the apps inside it are helpers.
            if let Some(app) = make_entry(path.to_string_lossy().into_owned(), None) {
                out.push(app);
            }
        } else {
            collect_bundles(&path, depth + 1, out);
        }
    }
}

/// Collapse the raw enumeration into the searchable index.
///
/// Two rules, in order:
/// 1. an exact duplicate path is always dropped (Spotlight can report the
///    same bundle twice);
/// 2. a same-named copy that is *not* installed under an Applications root is
///    dropped when an installed copy exists. Otherwise the copy in Downloads
///    can win the sort, the real app is filtered away by the apps-only mode
///    and the launcher is left with an installer while the installed app is
///    unreachable.
///
/// Copies that are all installed, or all outside an Applications root, are
/// kept: different bundles can legitimately share a display name.
fn dedup_entries(mut entries: Vec<AppEntry>) -> Vec<AppEntry> {
    entries.sort_by(|a, b| {
        a.display_name_lower
            .cmp(&b.display_name_lower)
            .then_with(|| b.in_app_dir.cmp(&a.in_app_dir))
    });

    let mut seen_paths: HashSet<String> = HashSet::new();
    entries.retain(|e| seen_paths.insert(e.path.clone()));

    let installed_names: HashSet<String> = entries
        .iter()
        .filter(|e| e.in_app_dir)
        .map(|e| e.display_name_lower.clone())
        .collect();
    entries.retain(|e| e.in_app_dir || !installed_names.contains(&e.display_name_lower));

    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attr_line_parsing() {
        let (path, display) =
            parse_attr_line("/Applications/Safari.app   kMDItemDisplayName = Safari浏览器")
                .unwrap();
        assert_eq!(path, "/Applications/Safari.app");
        assert_eq!(display.as_deref(), Some("Safari浏览器"));

        let (path, display) = parse_attr_line("/Applications/Foo.app").unwrap();
        assert_eq!(path, "/Applications/Foo.app");
        assert_eq!(display, None);
    }

    #[test]
    fn path_component_building() {
        // No localized FFI assertions here (locale-dependent); verify
        // structure: folders become components, app is last, flag set.
        let (components, in_app_dir) =
            build_path_components("/System/Applications/Utilities/Disk Utility.app", "磁盘工具");
        assert!(in_app_dir);
        assert_eq!(components.last().unwrap().name_lower, "磁盘工具");
        assert!(components.len() >= 2); // at least [Utilities?, 磁盘工具]

        let (components, in_app_dir) =
            build_path_components("/Applications/Safari.app", "Safari浏览器");
        assert!(in_app_dir);
        assert_eq!(components.len(), 1);

        let (_, in_app_dir) =
            build_path_components("/usr/libexec/SomeHelper.app", "SomeHelper");
        assert!(!in_app_dir);
    }

    #[test]
    fn installed_copy_survives_dedup() {
        let stray = AppEntry::with_display_name(
            "Foo".into(),
            "/Users/me/Downloads/Foo.app".into(),
            Some("Foo".into()),
        );
        let installed = AppEntry::with_display_name(
            "Foo".into(),
            "/Applications/Foo.app".into(),
            Some("Foo".into()),
        );

        // The stray copy must not shadow the installed one.
        let entries = dedup_entries(vec![stray, installed.clone()]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "/Applications/Foo.app");
        assert!(entries[0].in_app_dir, "apps-only mode must still find it");

        // Two installed copies are legitimately different bundles.
        let nested = AppEntry::with_display_name(
            "Foo".into(),
            "/Applications/Utilities/Foo.app".into(),
            Some("Foo".into()),
        );
        assert_eq!(dedup_entries(vec![installed.clone(), nested]).len(), 2);

        // Exact duplicate paths still collapse.
        assert_eq!(dedup_entries(vec![installed.clone(), installed]).len(), 1);
    }

    #[test]
    fn fallback_scan_finds_bundles_but_not_helpers_inside_them() {
        let dir = std::env::temp_dir().join("touchery-apps-scan-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("App.app/Contents/Helper.app")).unwrap();
        std::fs::create_dir_all(dir.join("Utilities/Nested.app")).unwrap();
        std::fs::write(dir.join("notes.txt"), "not an app").unwrap();

        let mut found = Vec::new();
        collect_bundles(&dir, 0, &mut found);
        let names: Vec<&str> = found.iter().map(|e| e.name.as_str()).collect();

        assert!(names.contains(&"App"), "{names:?}");
        assert!(names.contains(&"Nested"), "{names:?}");
        assert!(
            !names.contains(&"Helper"),
            "a bundle inside another bundle must not be listed: {names:?}"
        );
        assert_eq!(found.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finder_in_coreservices_is_user_facing() {
        // 访达 lives in CoreServices, not under an /Applications/ root.
        let (components, in_app_dir) =
            build_path_components("/System/Library/CoreServices/Finder.app", "访达");
        assert!(in_app_dir, "Finder must survive the apps-only filter");
        assert_eq!(components.len(), 1);
        assert_eq!(components[0].name_lower, "访达");

        // A genuine system helper outside every known user root stays out.
        let (_, in_app_dir) = build_path_components(
            "/System/Library/PrivateFrameworks/Something.framework/Versions/A/Helper.app",
            "Helper",
        );
        assert!(!in_app_dir);
    }
}
