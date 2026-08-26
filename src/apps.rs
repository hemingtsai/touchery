use std::process::Command;

use pinyin::ToPinyin;

#[derive(Debug, Clone)]
pub struct AppEntry {
    /// Bundle file stem, e.g. "WeChat" (fallback label + search field).
    pub name: String,
    /// Localized display name per the user's locale, e.g. "微信".
    pub display_name: String,
    pub path: String,
    pub display_name_lower: String,
    pub name_lower: String,
    /// Pinyin derived from the localized display name.
    pub pinyin_full: String,
    pub pinyin_initials: String,
}

impl AppEntry {
    fn new(name: String, path: String) -> Self {
        Self::with_display_name(name, path, None)
    }

    fn with_display_name(name: String, path: String, localized: Option<String>) -> Self {
        let display_name = localized.unwrap_or_else(|| {
            // Fallback when Spotlight gave us nothing: ask LaunchServices
            // directly (respects locale for bundled processes, may not for
            // unbundled ones — hence it is only a fallback).
            localized_display_name(&path).unwrap_or_else(|| name.clone())
        });
        let display_name_lower = display_name.to_lowercase();
        let name_lower = name.to_lowercase();

        let pinyin_vec: Vec<&str> = display_name
            .as_str()
            .to_pinyin()
            .flatten()
            .map(|p| p.plain())
            .collect();
        let pinyin_full: String = pinyin_vec.join("");
        let pinyin_initials: String =
            pinyin_vec.iter().filter_map(|s| s.chars().next()).collect();

        Self {
            name,
            display_name,
            path,
            display_name_lower,
            name_lower,
            pinyin_full,
            pinyin_initials,
        }
    }
}

/// Ask LaunchServices/NSFileManager for the localized display name of an app
/// bundle. Used only as a fallback when Spotlight is unavailable.
fn localized_display_name(path: &str) -> Option<String> {
    use objc::{class, msg_send, sel, sel_impl, runtime::Object};
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

const MARKER: &str = "kMDItemDisplayName = ";

/// Parse one line of `mdfind -attr kMDItemDisplayName` output:
/// `<path>   kMDItemDisplayName = <localized name>`
fn parse_attr_line(line: &str) -> Option<(String, Option<String>)> {
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
            Some((
                path,
                (!display.is_empty()).then_some(display),
            ))
        }
        None => Some((line.to_string(), None)),
    }
}

pub fn enumerate_apps() -> Vec<AppEntry> {
    const QUERY: &str = "kMDItemContentType == 'com.apple.application-bundle'";

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
                    let name = std::path::Path::new(&path)
                        .file_stem()?
                        .to_str()?
                        .to_string();
                    if name.starts_with('.') || name.contains("uninstal") {
                        return None;
                    }
                    Some(AppEntry::with_display_name(name, path, localized))
                })
                .collect()
        }
        _ => {
            // Spotlight unavailable/disabled: plain enumeration, names fall
            // back through NSFileManager.
            match Command::new("mdfind")
                .args(["kMDItemContentType", "==", "com.apple.application-bundle"])
                .output()
            {
                Ok(o) if o.status.success() => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    stdout
                        .lines()
                        .filter_map(|line| {
                            let path = line.trim().to_string();
                            if path.is_empty() {
                                return None;
                            }
                            let name = std::path::Path::new(&path)
                                .file_stem()?
                                .to_str()?
                                .to_string();
                            if name.starts_with('.') || name.contains("uninstal") {
                                return None;
                            }
                            Some(AppEntry::new(name, path))
                        })
                        .collect()
                }
                _ => Vec::new(),
            }
        }
    };

    // Sort by the visible (localized) name and drop duplicate bundles.
    entries.sort_by(|a, b| a.display_name_lower.cmp(&b.display_name_lower));
    entries.dedup_by(|a, b| a.path == b.path);
    entries.dedup_by(|a, b| a.display_name_lower == b.display_name_lower);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_attr_line_ok() {
        let (path, display) =
            parse_attr_line("/Applications/Safari.app   kMDItemDisplayName = Safari浏览器")
                .unwrap();
        assert_eq!(path, "/Applications/Safari.app");
        assert_eq!(display.as_deref(), Some("Safari浏览器"));
    }

    #[test]
    fn parse_plain_line() {
        let (path, display) = parse_attr_line("/Applications/Foo.app").unwrap();
        assert_eq!(path, "/Applications/Foo.app");
        assert_eq!(display, None);
    }
}

#[cfg(test)]
mod enum_tests {
    #[test]
    fn enumerate_shows_localized_names() {
        let apps = super::enumerate_apps();
        println!("total: {}", apps.len());
        let wanted = ["微信", "计算器", "Safari浏览器", "邮件", "终端"];
        for app in apps
            .iter()
            .filter(|a| wanted.contains(&a.display_name.as_str()))
            .take(8)
        {
            println!("{} | {} | pinyin: {} / {}", app.display_name, app.name, app.pinyin_full, app.pinyin_initials);
        }
        assert!(!apps.is_empty());
    }
}
