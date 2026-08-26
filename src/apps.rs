use std::collections::HashMap;
use std::process::Command;

use pinyin::{ToPinyin, ToPinyinMulti};

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

/// Localized folder-name cache shared across one enumeration run; most apps
/// share the same few ancestors (Utilities etc.), so we hit NSFileManager
/// once per distinct folder.
struct FolderLocalizer {
    cache: HashMap<String, String>,
}

impl FolderLocalizer {
    fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    fn localized(&mut self, folder_path: &str) -> String {
        if let Some(hit) = self.cache.get(folder_path) {
            return hit.clone();
        }
        let name = localized_display_name(folder_path)
            .unwrap_or_else(|| folder_name(&folder_path.to_string()));
        self.cache.insert(folder_path.to_string(), name.clone());
        name
    }
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
        // component (still slash-searchable), but flag it as out-of-scope.
        return (
            vec![PathComponent::new(app_display_name.to_string())],
            false,
        );
    };

    let mut localizer = FolderLocalizer::new();
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
        components.push(PathComponent::new(localizer.localized(&walked)));
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
    if name.starts_with('.') || name.contains("uninstal") {
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

    let mut entries: Vec<AppEntry> = match output {
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
            // Spotlight unavailable/disabled: plain enumeration, names fall
            // back through NSFileManager.
            match Command::new("mdfind").args(["kMDItemContentType", "==", QUERY]).output() {
                Ok(o) if o.status.success() => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    stdout
                        .lines()
                        .filter_map(|line| make_entry(line.trim().to_string(), None))
                        .collect()
                }
                _ => Vec::new(),
            }
        }
    };

    // Sort by the visible (localized) name, then drop duplicates in a single
    // pass: identical paths and identical display names are both adjacent
    // after this sort.
    entries.sort_by(|a, b| a.display_name_lower.cmp(&b.display_name_lower));
    entries.dedup_by(|a, b| a.display_name_lower == b.display_name_lower || a.path == b.path);
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
}
