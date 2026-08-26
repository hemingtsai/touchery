use std::process::Command;

use pinyin::ToPinyin;

#[derive(Debug, Clone)]
pub struct AppEntry {
    pub name: String,
    pub path: String,
    pub name_lower: String,
    pub pinyin_full: String,
    pub pinyin_initials: String,
}

impl AppEntry {
    fn new(name: String, path: String) -> Self {
        let name_lower = name.to_lowercase();
        let pinyin_vec: Vec<&str> = name.as_str()
            .to_pinyin()
            .flatten()
            .map(|p| p.plain())
            .collect();
        let pinyin_full: String = pinyin_vec.join("");
        let pinyin_initials: String = pinyin_vec
            .iter()
            .filter_map(|s| s.chars().next())
            .collect();
        Self {
            name,
            path,
            name_lower,
            pinyin_full,
            pinyin_initials,
        }
    }
}

pub fn enumerate_apps() -> Vec<AppEntry> {
    let output = Command::new("mdfind")
        .args([
            "kMDItemContentType",
            "==",
            "com.apple.application-bundle",
        ])
        .output();

    let stdout = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return Vec::new(),
    };

    let mut entries: Vec<AppEntry> = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let path = line.trim().to_string();
            let name = std::path::Path::new(&path)
                .file_stem()?
                .to_str()?
                .to_string();
            if name.starts_with('.') || name.contains("uninstal") {
                return None;
            }
            Some(AppEntry::new(name, path))
        })
        .collect();

    entries.sort_by(|a, b| a.name_lower.cmp(&b.name_lower));
    entries.dedup_by(|a, b| a.path == b.path);
    entries
}
