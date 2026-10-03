// objc 0.2's class!/msg_send! macros internally check cfg(feature = "cargo-clippy"),
// which cargo cannot know about; silence the resulting false-positive lints.
#![allow(unexpected_cfgs)]
use std::collections::{HashMap, HashSet};
use std::process::Command;

use pinyin::ToPinyinMulti;

// ---------------------------------------------------------------------------
// Search keys
// ---------------------------------------------------------------------------

/// Which field a search key came from. The weight is the ranking preference
/// applied to a similarity score, in thousandths: a bundle-name match ranks
/// just below an equally good match on the display name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    DisplayFull,
    DisplayAbbr,
    BundleFull,
    FolderFull,
    FolderAbbr,
}

impl KeyKind {
    pub fn weight(self) -> u32 {
        match self {
            KeyKind::BundleFull => 980,
            KeyKind::DisplayFull
            | KeyKind::DisplayAbbr
            | KeyKind::FolderFull
            | KeyKind::FolderAbbr => 1000,
        }
    }
}

/// A spelling under construction: folded bytes, each flagged when it starts a
/// word.
type Marked = Vec<(u8, bool)>;

/// Max number of cartesian-product variants per string. App names are short;
/// polyphonic characters multiply candidates but stay far below this.
const MAX_VARIANTS: usize = 64;

/// A prepared search key.
///
/// Everything is a-z0-9: Chinese characters are expanded to pinyin at index
/// time and Latin accents are folded to their base letter, so the scorer never
/// touches Unicode and can reject impossible matches with a bitmask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchKey {
    pub chars: Box<[u8]>,
    /// One bit per character present: 0-25 for a-z, 26-35 for 0-9.
    pub mask: u64,
    /// Positions in `chars` that count as word starts; always includes 0.
    pub starts: Box<[u32]>,
    pub kind: KeyKind,
}

/// Mask slot of a folded byte.
pub(crate) fn mask_slot(byte: u8) -> u32 {
    debug_assert!(byte.is_ascii_digit() || byte.is_ascii_lowercase());
    if byte.is_ascii_digit() {
        26 + u32::from(byte - b'0')
    } else {
        u32::from(byte - b'a')
    }
}

/// Fold a character to its lowercase ASCII base letter or digit. Accented
/// Latin letters (é → e) are folded so a Latin keyboard still matches a
/// localized name; anything else (Chinese, punctuation, space) yields `None`.
pub(crate) fn ascii_fold(ch: char) -> Option<u8> {
    let lower = ch.to_lowercase().next()?;
    if lower.is_ascii() {
        return lower.is_ascii_alphanumeric().then_some(lower as u8);
    }
    Some(match lower {
        'ß' => b's',
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => b'a',
        'ç' | 'ć' | 'č' => b'c',
        'ď' | 'đ' => b'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => b'e',
        'ğ' | 'ģ' => b'g',
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ĭ' | 'į' | 'ı' => b'i',
        'ł' | 'ĺ' | 'ľ' | 'ļ' => b'l',
        'ñ' | 'ń' | 'ň' | 'ņ' => b'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => b'o',
        'ŕ' | 'ř' | 'ŗ' => b'r',
        'ś' | 'š' | 'ş' | 'ș' => b's',
        'ť' | 'ţ' | 'ț' => b't',
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => b'u',
        'ý' | 'ÿ' | 'ŷ' => b'y',
        'ź' | 'ż' | 'ž' => b'z',
        _ => return None,
    })
}

impl SearchKey {
    fn from_marked(marked: Marked, kind: KeyKind) -> Option<Self> {
        if marked.is_empty() {
            return None;
        }
        let mut mask = 0u64;
        let mut starts = Vec::new();
        let mut chars = Vec::with_capacity(marked.len());
        for (index, (byte, starts_word)) in marked.into_iter().enumerate() {
            mask |= 1u64 << mask_slot(byte);
            if index == 0 || starts_word {
                starts.push(index as u32);
            }
            chars.push(byte);
        }
        Some(Self {
            chars: chars.into_boxed_slice(),
            mask,
            starts: starts.into_boxed_slice(),
            kind,
        })
    }
}

/// Cartesian product of the per-character alternatives, capped.
fn variants_product(alternatives: Vec<Vec<Marked>>) -> Vec<Marked> {
    let mut acc: Vec<Marked> = vec![Vec::new()];
    for alternatives in alternatives {
        if alternatives.is_empty() {
            continue; // character contributes nothing
        }
        let mut next = Vec::with_capacity(acc.len() * alternatives.len());
        for prefix in &acc {
            for alternative in &alternatives {
                let mut combined = prefix.clone();
                combined.extend_from_slice(alternative);
                next.push(combined);
            }
        }
        if next.len() > MAX_VARIANTS {
            next.truncate(MAX_VARIANTS);
        }
        acc = next;
    }
    acc
}

/// Every spelling of `text`: the full one (Chinese characters expanded to
/// pinyin, polyphonic characters contributing each reading) and the
/// abbreviation (one letter per Chinese character, one per Latin word).
fn latin_variants(text: &str) -> (Vec<Marked>, Vec<Marked>) {
    let mut full_alternatives: Vec<Vec<Marked>> = Vec::new();
    let mut abbr_alternatives: Vec<Vec<Marked>> = Vec::new();
    let mut next_starts_word = true;
    let mut previous_lower_or_digit = false;

    for ch in text.chars() {
        if let Some(readings) = ch.to_pinyin_multi() {
            let mut fulls = Vec::new();
            let mut initials = Vec::new();
            for reading in readings {
                let mut chars: Marked = Vec::new();
                for (index, c) in reading.plain().chars().enumerate() {
                    if let Some(byte) = ascii_fold(c) {
                        chars.push((byte, index == 0 && next_starts_word));
                    }
                }
                if chars.is_empty() {
                    continue;
                }
                initials.push(vec![(chars[0].0, true)]);
                fulls.push(chars);
            }
            if !fulls.is_empty() {
                full_alternatives.push(fulls);
                abbr_alternatives.push(initials);
            }
            // A Chinese character is a syllable of its own: what follows does
            // not start a new word unless a separator says so.
            next_starts_word = false;
            previous_lower_or_digit = false;
            continue;
        }

        match ascii_fold(ch) {
            Some(byte) => {
                let starts_word =
                    next_starts_word || (ch.is_uppercase() && previous_lower_or_digit);
                full_alternatives.push(vec![vec![(byte, starts_word)]]);
                // Latin characters only reach the abbreviation at the start of
                // a word: "Disk Utility" → "du", not "diskutility".
                abbr_alternatives.push(vec![if starts_word {
                    vec![(byte, true)]
                } else {
                    Vec::new()
                }]);
                next_starts_word = false;
                previous_lower_or_digit = ch.is_lowercase() || ch.is_ascii_digit();
            }
            None => {
                next_starts_word = true;
                previous_lower_or_digit = false;
            }
        }
    }

    (
        variants_product(full_alternatives),
        variants_product(abbr_alternatives),
    )
}

/// Add `key` unless the same spelling is already present: the first kind wins,
/// which keeps a display-name key ahead of an identical bundle-name key.
fn push_unique_key(keys: &mut Vec<SearchKey>, key: SearchKey) {
    if keys.iter().any(|existing| existing.chars == key.chars) {
        return;
    }
    keys.push(key);
}

/// Search keys of one name: its full spellings plus, optionally, its
/// abbreviation.
fn keys_for_name(text: &str, full_kind: KeyKind, abbr_kind: Option<KeyKind>) -> Vec<SearchKey> {
    let (fulls, abbrs) = latin_variants(text);
    let mut keys = Vec::new();
    for marked in fulls {
        if let Some(key) = SearchKey::from_marked(marked, full_kind) {
            push_unique_key(&mut keys, key);
        }
    }
    if let Some(kind) = abbr_kind {
        for marked in abbrs {
            // A one-letter abbreviation only repeats the first letter of the
            // full spelling, so it would cost work without adding matches.
            if marked.len() < 2 {
                continue;
            }
            if let Some(key) = SearchKey::from_marked(marked, kind) {
                push_unique_key(&mut keys, key);
            }
        }
    }
    keys
}

#[derive(Debug, Clone)]
pub struct AppEntry {
    /// Bundle file stem, e.g. "WeChat" (fallback label + search field).
    pub name: String,
    /// Localized display name per the user's locale, e.g. "微信".
    pub display_name: String,
    pub path: String,
    /// Lowercased display name; used to collapse duplicate copies.
    pub display_name_lower: String,
    /// Prepared keys of the app itself: the display name (full spelling and
    /// abbreviation) and the bundle name.
    pub keys: Vec<SearchKey>,
    /// Prepared keys of each ancestor folder, outermost first. Only queries
    /// containing '/' consult these.
    pub folder_keys: Vec<Vec<SearchKey>>,
    /// True when the bundle lives under an Applications root (/Applications,
    /// /System/Applications, ~/Applications, ...) — i.e. a user-facing app.
    pub in_app_dir: bool,
}

impl AppEntry {
    pub(crate) fn with_display_name(name: String, path: String, localized: Option<String>) -> Self {
        let display_name = localized.unwrap_or_else(|| {
            // Fallback when Spotlight gave us nothing: ask LaunchServices
            // directly (respects locale for bundled processes, may not for
            // unbundled ones — hence it is only a fallback).
            localized_display_name(&path).unwrap_or_else(|| name.clone())
        });
        let display_name_lower = display_name.to_lowercase();

        // The bundle name only speaks for itself, never as an abbreviation:
        // "WeChat" is typed in full.
        let mut keys = keys_for_name(
            &display_name,
            KeyKind::DisplayFull,
            Some(KeyKind::DisplayAbbr),
        );
        for key in keys_for_name(&name, KeyKind::BundleFull, None) {
            push_unique_key(&mut keys, key);
        }

        let (folder_names, in_app_dir) = build_folder_names(&path);
        let folder_keys = folder_names
            .iter()
            .map(|folder| keys_for_name(folder, KeyKind::FolderFull, Some(KeyKind::FolderAbbr)))
            .collect();

        Self {
            name,
            display_name,
            path,
            display_name_lower,
            keys,
            folder_keys,
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

/// Localized names of the folders between the nearest Applications root and
/// the app bundle, outermost first, plus whether the bundle counts as
/// user-facing.
fn build_folder_names(bundle_path: &str) -> (Vec<String>, bool) {
    let Some(root_idx) = bundle_path.find(APPS_MARKER) else {
        // Not under an Applications root: no ancestors to search. Apps under a
        // known user-facing root (CoreServices: Finder & friends) still count
        // as in-scope; anything else is flagged out-of-scope.
        let in_app_dir = EXTRA_USER_ROOTS
            .iter()
            .any(|root| bundle_path.starts_with(root));
        return (Vec::new(), in_app_dir);
    };

    let mut folders = Vec::new();

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
        folders.push(localized_folder(&walked));
    }

    (folders, true)
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
    let mut command = Command::new("mdfind");
    command.args(["-attr", "kMDItemDisplayName", QUERY]);

    let entries: Vec<AppEntry> = match output_with_timeout(&mut command, MDFIND_TIMEOUT) {
        Some(o) if o.status.success() && !o.stdout.is_empty() => {
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
            // Spotlight unavailable, disabled, still indexing or too slow:
            // walk the known application roots. Repeating the same mdfind
            // query cannot help — it fails for exactly the same reason the
            // first one did.
            eprintln!("[apps] Spotlight returned no applications; scanning the filesystem instead");
            scan_app_roots()
        }
    };

    dedup_entries(entries)
}

/// How long `mdfind` may take before the enumeration falls back to walking the
/// filesystem. Spotlight normally answers in milliseconds; a stuck query would
/// otherwise leave the index empty and the re-index in flight forever.
const MDFIND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Run `command` and collect its stdout, killing it once `timeout` elapses.
///
/// Returns `None` when the command cannot be started, exits unsuccessfully or
/// outlives the timeout. Output is redirected to a temporary file rather than a
/// pipe: while polling for exit nothing drains a pipe, so a child that fills
/// the pipe buffer would deadlock instead of timing out.
fn output_with_timeout(
    command: &mut Command,
    timeout: std::time::Duration,
) -> Option<std::process::Output> {
    use std::process::Stdio;

    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "touchery-command-{}-{sequence}.txt",
        std::process::id()
    ));
    let file = std::fs::File::create(&path).ok()?;

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(_) => break None,
        }
    };

    let stdout = std::fs::read(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);

    status.map(|status| std::process::Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
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
    fn folder_names_follow_the_path() {
        // No localized-name assertions here (locale-dependent); verify the
        // structure instead: ancestors are listed outermost first.
        let (folders, in_app_dir) =
            build_folder_names("/System/Applications/Utilities/Disk Utility.app");
        assert!(in_app_dir);
        assert_eq!(folders.len(), 1);

        let (folders, in_app_dir) = build_folder_names("/Applications/Safari.app");
        assert!(in_app_dir);
        assert!(folders.is_empty(), "no ancestors below the root");

        let (folders, in_app_dir) = build_folder_names("/usr/libexec/SomeHelper.app");
        assert!(!in_app_dir);
        assert!(folders.is_empty());
    }

    fn key_spellings(text: &str, kind: KeyKind) -> Vec<String> {
        keys_for_name(text, kind, None)
            .iter()
            .map(|key| key.chars.iter().map(|byte| *byte as char).collect())
            .collect()
    }

    #[test]
    fn latin_names_keep_their_letters_and_drop_redundant_abbreviations() {
        let entry = AppEntry::with_display_name(
            "Safari".into(),
            "/Applications/Safari.app".into(),
            Some("Safari".into()),
        );
        let spellings: Vec<String> = entry
            .keys
            .iter()
            .map(|key| key.chars.iter().map(|byte| *byte as char).collect())
            .collect();

        // The display key and the identical bundle key are the same spelling,
        // so only one survives, with the display name's kind.
        assert_eq!(spellings, vec!["safari".to_string()]);
        assert_eq!(entry.keys[0].kind, KeyKind::DisplayFull);
        // A one-letter abbreviation only repeats the first letter.
        assert!(!spellings.contains(&"s".to_string()));
    }

    #[test]
    fn han_names_become_pinyin_plus_abbreviations() {
        let entry = AppEntry::with_display_name(
            "WeChat".into(),
            "/Applications/WeChat.app".into(),
            Some("微信".into()),
        );
        let by_kind = |kind: KeyKind| -> Vec<String> {
            entry
                .keys
                .iter()
                .filter(|key| key.kind == kind)
                .map(|key| key.chars.iter().map(|byte| *byte as char).collect())
                .collect()
        };

        assert!(by_kind(KeyKind::DisplayFull).contains(&"weixin".to_string()));
        assert!(by_kind(KeyKind::DisplayAbbr).contains(&"wx".to_string()));
        assert!(by_kind(KeyKind::BundleFull).contains(&"wechat".to_string()));
    }

    #[test]
    fn polyphonic_readings_are_all_indexed() {
        assert!(key_spellings("音乐", KeyKind::DisplayFull).contains(&"yinyue".to_string()));
        assert!(key_spellings("音乐", KeyKind::DisplayFull).contains(&"yinle".to_string()));

        let initials: Vec<String> =
            keys_for_name("音乐", KeyKind::DisplayAbbr, Some(KeyKind::DisplayAbbr))
                .iter()
                .map(|key| key.chars.iter().map(|byte| *byte as char).collect())
                .collect();
        assert!(initials.contains(&"yy".to_string()), "{initials:?}");
        assert!(initials.contains(&"yl".to_string()), "{initials:?}");
    }

    #[test]
    fn word_starts_survive_separators_and_camel_case() {
        let (fulls, abbrs) = latin_variants("Disk Utility");
        let spelling =
            |marked: &Marked| -> String { marked.iter().map(|(byte, _)| *byte as char).collect() };
        let starts = |marked: &Marked| -> Vec<usize> {
            marked
                .iter()
                .enumerate()
                .filter(|(_, (_, starts_word))| *starts_word)
                .map(|(index, _)| index)
                .collect()
        };

        let full = fulls
            .iter()
            .find(|marked| spelling(marked) == "diskutility")
            .expect("full spelling");
        assert_eq!(starts(full), vec![0, 4]);
        assert_eq!(spelling(&abbrs[0]), "du");

        let (camel, _) = latin_variants("WeChat");
        let full = camel
            .iter()
            .find(|marked| spelling(marked) == "wechat")
            .expect("full spelling");
        assert_eq!(starts(full), vec![0, 2]);
    }

    #[test]
    fn masks_and_starts_cover_every_character() {
        let key = &keys_for_name("A1b", KeyKind::DisplayFull, None)[0];
        assert_eq!(&*key.chars, b"a1b");
        assert_eq!(key.mask.count_ones(), 3);
        assert!(key.mask & (1u64 << mask_slot(b'a')) != 0);
        assert!(key.mask & (1u64 << mask_slot(b'1')) != 0);
        assert_eq!(&*key.starts, &[0]);
    }

    #[test]
    fn folder_keys_are_built_from_the_ancestors() {
        let entry = AppEntry::with_display_name(
            "My App".into(),
            "/Applications/Dev Tools/My App.app".into(),
            Some("My App".into()),
        );
        assert_eq!(entry.folder_keys.len(), 1);
        let folder: Vec<String> = entry.folder_keys[0]
            .iter()
            .map(|key| key.chars.iter().map(|byte| *byte as char).collect())
            .collect();
        assert!(folder.contains(&"devtools".to_string()), "{folder:?}");
        assert!(folder.contains(&"dt".to_string()), "{folder:?}");
        assert!(
            entry.folder_keys[0]
                .iter()
                .all(|key| matches!(key.kind, KeyKind::FolderFull | KeyKind::FolderAbbr))
        );
    }

    #[test]
    fn names_without_an_applications_root_have_no_folder_keys() {
        let entry = AppEntry::with_display_name(
            "Helper".into(),
            "/usr/libexec/Helper.app".into(),
            Some("Helper".into()),
        );
        assert!(entry.folder_keys.is_empty());
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
    fn external_commands_are_bounded_by_a_timeout() {
        let mut fast = Command::new("/bin/echo");
        fast.arg("hello");
        let output = output_with_timeout(&mut fast, std::time::Duration::from_secs(5))
            .expect("echo must run");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"hello\n");

        let mut slow = Command::new("/bin/sleep");
        slow.arg("30");
        let started = std::time::Instant::now();
        assert!(
            output_with_timeout(&mut slow, std::time::Duration::from_millis(200)).is_none(),
            "a command that outlives its timeout must be killed"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
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
        let (folders, in_app_dir) = build_folder_names("/System/Library/CoreServices/Finder.app");
        assert!(in_app_dir, "Finder must survive the apps-only filter");
        assert!(
            folders.is_empty(),
            "CoreServices is a root, not an ancestor"
        );

        // A genuine system helper outside every known user root stays out.
        let (_, in_app_dir) = build_folder_names(
            "/System/Library/PrivateFrameworks/Something.framework/Versions/A/Helper.app",
        );
        assert!(!in_app_dir);
    }
}
