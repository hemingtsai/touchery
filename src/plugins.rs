use crate::config::Config;
use crate::lua_budget;
use anyhow::Context as _;
use mlua::{Function, Lua, Table};
use std::path::{Path, PathBuf};

/// The running build, which plugin version ranges are checked against.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Directory inside the plugins directory that holds plugins this build cannot
/// run.
pub const LEGACY_DIR: &str = "legacy";

/// A single item produced by a plugin's `get_items`.
#[derive(Debug, Clone)]
pub struct PluginItem {
    pub title: String,
    pub value: String,
    /// If true, Enter opens the secondary-input flow (`run_sub`).
    pub sub: bool,
}

/// Optional self-description a plugin may declare:
///
/// ```lua
/// PLUGIN = {
///   name = "计算器", version = "1.0.0", author = "…", license = "MIT",
///   repository = "https://…", description = "…",
///   min_touchery = "1.3.0", max_touchery = "2.0.0",
/// }
/// ```
///
/// Every field is optional: `name` falls back to the file stem and the version
/// bounds are open. The table has to be a *literal* (`scan_metadata` reads the
/// source instead of running it), values have to be quoted strings, and the
/// repository is only kept when it is an http(s) URL — it is handed to `open`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginInfo {
    pub name: String,
    pub version: Option<String>,
    pub author: Option<String>,
    pub license: Option<String>,
    pub repository: Option<String>,
    pub description: Option<String>,
    /// Oldest Touchery this plugin supports, inclusive.
    pub min_touchery: Option<String>,
    /// Newest Touchery this plugin supports, inclusive.
    pub max_touchery: Option<String>,
}

impl PluginInfo {
    pub fn with_name(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Self::default()
        }
    }
}

/// Longest accepted metadata string, per field.
const INFO_MAX: usize = 120;

// ---------------------------------------------------------------------------
// Reading the `PLUGIN` table without running the plugin
// ---------------------------------------------------------------------------

/// Read the literal `PLUGIN = { … }` table out of a plugin's source.
///
/// The source is scanned rather than executed on purpose. A plugin whose range
/// excludes this build has to be parked *without* its chunk ever running, and
/// the install path is derived from the metadata before the file is copied.
///
/// Only a literal table with quoted string values is understood — what the
/// README documents. A computed table, or a field built with `..`, reads as
/// "not declared", which keeps a plugin installable and loadable.
pub fn scan_metadata(source: &str, fallback_name: &str) -> PluginInfo {
    let mut info = PluginInfo::with_name(fallback_name);
    let Some(body) = literal_table_body(source, "PLUGIN") else {
        return info;
    };
    let fields = literal_fields(body);
    let field = |key: &str| -> Option<String> {
        fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.trim().chars().take(INFO_MAX).collect::<String>())
            .filter(|value| !value.is_empty())
    };

    if let Some(name) = field("name") {
        info.name = name;
    }
    info.version = field("version");
    info.author = field("author");
    info.license = field("license");
    info.description = field("description");
    info.repository =
        field("repository").filter(|url| url.starts_with("https://") || url.starts_with("http://"));
    info.min_touchery = field("min_touchery").filter(|text| version_tuple(text).is_some());
    info.max_touchery = field("max_touchery").filter(|text| version_tuple(text).is_some());
    info
}

/// Body of the `KEY = { … }` table in `source`, or `None` when there is none.
fn literal_table_body<'a>(source: &'a str, key: &str) -> Option<&'a str> {
    find_declaration(source, key).map(|open| braces_body(source, open))
}

/// Byte offset of the `{` in the first `KEY = {` that is neither inside a
/// string nor inside a comment.
fn find_declaration(source: &str, key: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    let key = key.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' | b'\'' => i = skip_quoted(bytes, i),
            b'[' => i = skip_long_bracket(bytes, i),
            c if (c.is_ascii_alphabetic() || c == b'_') && bytes[i..].starts_with(key) => {
                let after = i + key.len();
                let before_ok = i == 0 || !is_word_byte(bytes[i - 1]);
                let after_ok = !bytes.get(after).is_some_and(|byte| is_word_byte(*byte));
                if before_ok && after_ok {
                    let mut j = after;
                    while bytes.get(j).is_some_and(u8::is_ascii_whitespace) {
                        j += 1;
                    }
                    if bytes.get(j) == Some(&b'=') {
                        j += 1;
                        while bytes.get(j).is_some_and(u8::is_ascii_whitespace) {
                            j += 1;
                        }
                        if bytes.get(j) == Some(&b'{') {
                            return Some(j);
                        }
                    }
                }
                i = after;
            }
            _ => i += 1,
        }
    }
    None
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'
}

/// Source of the `{ … }` table whose opening brace sits at `open`, braces
/// inside strings and comments ignored.
fn braces_body(source: &str, open: usize) -> &str {
    let bytes = source.as_bytes();
    let start = open + 1;
    let mut i = start;
    let mut depth = 1i32;
    while i < bytes.len() {
        match bytes[i] {
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' | b'\'' => i = skip_quoted(bytes, i),
            b'[' => i = skip_long_bracket(bytes, i),
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[start..i];
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    &source[start..]
}

/// `key = "value"` pairs at the top level of a table body.
fn literal_fields(body: &str) -> Vec<(String, String)> {
    let bytes = body.as_bytes();
    let mut fields = Vec::new();
    let mut i = 0;
    let mut depth = 0i32;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' | b'\'' => i = skip_quoted(bytes, i),
            b'[' => i = skip_long_bracket(bytes, i),
            c if depth == 0 && (c.is_ascii_alphabetic() || c == b'_') => {
                let key_start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let key = &body[key_start..i];
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if bytes.get(i) != Some(&b'=') {
                    continue;
                }
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if matches!(bytes.get(i), Some(b'"') | Some(b'\'')) {
                    let (value, next) = read_quoted(bytes, i);
                    // `"a" .. "b"` is a computed value, not a literal one: the
                    // field counts as not declared.
                    let mut j = next;
                    while bytes.get(j).is_some_and(u8::is_ascii_whitespace) {
                        j += 1;
                    }
                    if bytes[j..].starts_with(b"..") {
                        i = j + 2;
                    } else {
                        fields.push((key.to_string(), value));
                        i = next;
                    }
                }
                // Anything else (a number, a table, an expression) is skipped.
            }
            _ => i += 1,
        }
    }
    fields
}

/// Index just past the quoted string that starts at `open`.
fn skip_quoted(bytes: &[u8], open: usize) -> usize {
    let quote = bytes[open];
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            c if c == quote => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

/// Read the quoted string starting at `open`: its value and the index past it.
fn read_quoted(bytes: &[u8], open: usize) -> (String, usize) {
    let quote = bytes[open];
    let mut value = Vec::new();
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                let escaped = bytes[i + 1];
                value.push(match escaped {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'r' => b'\r',
                    other => other,
                });
                i += 2;
            }
            c if c == quote => return (String::from_utf8_lossy(&value).into_owned(), i + 1),
            c => {
                value.push(c);
                i += 1;
            }
        }
    }
    (String::from_utf8_lossy(&value).into_owned(), bytes.len())
}

/// Index just past the `[[…]]` / `[=[…]=]` long bracket at `open`; an ordinary
/// index like `t[1]` is left alone.
fn skip_long_bracket(bytes: &[u8], open: usize) -> usize {
    let mut i = open + 1;
    let mut level = 0usize;
    while bytes.get(i) == Some(&b'=') {
        level += 1;
        i += 1;
    }
    if bytes.get(i) != Some(&b'[') {
        return open + 1;
    }
    i += 1;
    while i < bytes.len() {
        if bytes[i] == b']' {
            let mut j = i + 1;
            let mut seen = 0usize;
            while bytes.get(j) == Some(&b'=') {
                seen += 1;
                j += 1;
            }
            if seen == level && bytes.get(j) == Some(&b']') {
                return j + 1;
            }
        }
        i += 1;
    }
    bytes.len()
}

// ---------------------------------------------------------------------------
// Version ranges
// ---------------------------------------------------------------------------

/// Numeric parts of a version: `1.3.0-beta` -> `(1, 3, 0)`.
///
/// Pre-release and build suffixes are dropped on purpose, so the beta series
/// satisfies `min_touchery = "1.3.0"` instead of failing an exact comparison.
pub fn version_tuple(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text
        .trim()
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty());
    let major: u32 = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    let patch = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

/// Whether `current` lies inside the range the plugin declares. A missing bound
/// is open, and an unparseable one (already dropped by `scan_metadata`) is
/// treated as absent.
pub fn supports(info: &PluginInfo, current: &str) -> bool {
    let Some(current) = version_tuple(current) else {
        return true;
    };
    if let Some(min) = info.min_touchery.as_deref().and_then(version_tuple)
        && current < min
    {
        return false;
    }
    if let Some(max) = info.max_touchery.as_deref().and_then(version_tuple)
        && current > max
    {
        return false;
    }
    true
}

/// `1.3.0 – 2.0.0`, `≥ 1.3.0`, `≤ 2.0.0` or nothing.
pub fn range_text(info: &PluginInfo) -> Option<String> {
    match (&info.min_touchery, &info.max_touchery) {
        (Some(min), Some(max)) => Some(format!("{min} – {max}")),
        (Some(min), None) => Some(format!("≥ {min}")),
        (None, Some(max)) => Some(format!("≤ {max}")),
        (None, None) => None,
    }
}

/// Why a plugin is parked, in words the control panel can show.
pub fn range_reason(info: &PluginInfo, current: &str) -> String {
    match range_text(info) {
        Some(range) => format!("它需要 Touchery {range}，当前是 {current}"),
        None => format!("与当前版本 {current} 不兼容"),
    }
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// One path component derived from user text: path separators and control
/// characters become `-`, surrounding dots and spaces go, length is capped.
fn sanitize_component(text: &str, max: usize) -> String {
    text.trim()
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '\0') {
                '-'
            } else {
                c
            }
        })
        .take(max)
        .collect::<String>()
        .trim_matches(['.', ' '])
        .to_string()
}

/// Where a script with this metadata belongs, relative to the plugins
/// directory: `<author>/<name>-<version>.lua`.
fn layout_path(info: &PluginInfo, source_stem: &str) -> String {
    let author = sanitize_component(info.author.as_deref().unwrap_or_default(), 60);
    let author = if author.is_empty() {
        "unknown".to_string()
    } else {
        author
    };
    let name = sanitize_component(&info.name, 60);
    let name = if name.is_empty() {
        sanitize_component(source_stem, 60)
    } else {
        name
    };
    let name = if name.is_empty() {
        "plugin".to_string()
    } else {
        name
    };
    match info
        .version
        .as_deref()
        .map(|version| sanitize_component(version, 30))
        .filter(|version| !version.is_empty())
    {
        Some(version) => format!("{author}/{name}-{version}.lua"),
        None => format!("{author}/{name}.lua"),
    }
}

/// Identity used to tell two versions of the same plugin apart from two
/// different plugins: the author plus the declared name, case-insensitive.
fn plugin_key(info: &PluginInfo) -> String {
    let author = sanitize_component(info.author.as_deref().unwrap_or_default(), 60).to_lowercase();
    format!("{author}/{}", info.name.trim().to_lowercase())
}

/// File stem without the `-<version>` an installation appends:
/// `hemingtsai/计算器-1.0.0.lua` -> `计算器`, `calc.lua` -> `calc`.
fn stem_without_version(relative: &str) -> String {
    let name = Path::new(relative)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let stem = name.strip_suffix(".lua").unwrap_or(&name).to_string();
    // Search from the right: a pre-release suffix contains hyphens of its own
    // (`计算器-1.0.0-beta`), so the first hyphen followed by digits and a
    // parseable version is the one that separates name from version.
    for (index, _) in stem.match_indices('-').rev() {
        if index == 0 {
            continue;
        }
        let tail = &stem[index + 1..];
        if tail.chars().next().is_some_and(|c| c.is_ascii_digit()) && version_tuple(tail).is_some()
        {
            return stem[..index].to_string();
        }
    }
    stem
}

/// Plugin identity from its relative path: `a/b.lua` -> `a/b`.
fn plugin_identity(relative: &str) -> String {
    relative
        .strip_suffix(".lua")
        .unwrap_or(relative)
        .to_string()
}

/// Reject a path that would escape the plugins directory.
fn safe_relative(file_name: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(file_name);
    if path.is_absolute() {
        anyhow::bail!("路径必须是相对路径");
    }
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(name) => clean.push(name),
            std::path::Component::CurDir => {}
            _ => anyhow::bail!("路径不能包含 .. 或根目录"),
        }
    }
    if clean.as_os_str().is_empty() {
        anyhow::bail!("空路径");
    }
    Ok(clean)
}

/// Move a file inside the plugins directory, creating the target directory.
fn move_file(root: &Path, from: &str, to: &str) -> anyhow::Result<()> {
    let from = root.join(safe_relative(from)?);
    let to = root.join(safe_relative(to)?);
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("无法创建 {}", parent.display()))?;
    }
    std::fs::rename(&from, &to)
        .with_context(|| format!("无法移动 {} 到 {}", from.display(), to.display()))?;
    prune_empty_dirs(root, from.parent());
    Ok(())
}

/// Delete one file inside the plugins directory, pruning what it leaves empty.
fn remove_file(root: &Path, file_name: &str) -> anyhow::Result<()> {
    let target = root.join(safe_relative(file_name)?);
    std::fs::remove_file(&target).with_context(|| format!("无法删除 {}", target.display()))?;
    prune_empty_dirs(root, target.parent());
    Ok(())
}

/// Remove directories left empty by a move or a delete, up to the plugins
/// directory itself.
fn prune_empty_dirs(root: &Path, mut dir: Option<&Path>) {
    while let Some(current) = dir {
        if current == root || !current.starts_with(root) {
            return;
        }
        let empty = std::fs::read_dir(current)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false);
        if !empty || std::fs::remove_dir(current).is_err() {
            return;
        }
        dir = current.parent();
    }
}

/// Plugin files under `root`, relative to it with `/` separators. Recurses a
/// few levels (the `<author>/` layout) and skips hidden directories, plus
/// `legacy` when asked to.
fn discover(root: &Path, skip_legacy: bool) -> Vec<String> {
    const MAX_DEPTH: usize = 4;
    let mut found = Vec::new();
    collect_lua(root, root, MAX_DEPTH, skip_legacy, &mut found);
    found.sort();
    found
}

fn collect_lua(root: &Path, dir: &Path, depth: usize, skip_legacy: bool, found: &mut Vec<String>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        let Some(name) = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
        else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if skip_legacy && name == LEGACY_DIR {
                continue;
            }
            collect_lua(root, &path, depth - 1, false, found);
        } else if path.extension().is_some_and(|ext| ext == "lua")
            && let Ok(relative) = path.strip_prefix(root)
        {
            found.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

// ---------------------------------------------------------------------------
// Installing
// ---------------------------------------------------------------------------

/// A script that has been copied into the plugins directory.
#[derive(Debug, Clone)]
pub struct Installed {
    /// Path relative to the plugins directory.
    pub file_name: String,
    pub info: PluginInfo,
    /// The same `<author>/<name>-<version>.lua` was already there and has been
    /// replaced (reinstalling that exact version).
    pub replaced: bool,
    /// Stem of the file the user picked. A plugin installed earlier under
    /// another name or in the flat layout is still recognised as the same one.
    pub source_stem: String,
}

/// Install a Lua script as `<author>/<name>-<version>.lua`, both taken from the
/// script's own metadata (`unknown`/no suffix when it declares none).
pub fn install_script(source: &Path) -> anyhow::Result<Installed> {
    let dir = plugins_dir().context("no data directory available")?;
    install_into(&dir, source)
}

fn install_into(dir: &Path, source: &Path) -> anyhow::Result<Installed> {
    let source_name = source
        .file_name()
        .context("no file name")?
        .to_string_lossy()
        .to_string();
    if !source_name.to_ascii_lowercase().ends_with(".lua") {
        anyhow::bail!("只能安装 .lua 文件（收到 {source_name}）");
    }
    // Read it first: a directory, a binary or a non-UTF-8 file is refused
    // before anything lands in the plugins directory.
    let text = std::fs::read_to_string(source)
        .with_context(|| format!("无法读取 {}", source.display()))?;
    if text.trim().is_empty() {
        anyhow::bail!("{source_name} 是空文件");
    }

    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_default();
    let info = scan_metadata(&text, &stem);
    let file_name = layout_path(&info, &stem);
    let target = dir.join(safe_relative(&file_name)?);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("无法创建 {}", parent.display()))?;
    }
    let replaced = target.exists();
    // Copy the bytes rather than the decoded text so line endings survive
    // exactly.
    std::fs::copy(source, &target).with_context(|| format!("无法写入 {}", target.display()))?;
    Ok(Installed {
        file_name,
        info,
        replaced,
        source_stem: stem,
    })
}

/// Directory containing user plugins. Returns `None` when no data directory
/// can be determined — plugin loading is then disabled entirely (never falls
/// back to the current working directory, which would read arbitrary code
/// relative to wherever the binary was launched).
pub fn plugins_dir() -> Option<PathBuf> {
    crate::config::data_root().map(|root| root.join("plugins"))
}

// ---------------------------------------------------------------------------
// Plugins
// ---------------------------------------------------------------------------

pub struct Plugin {
    /// Path relative to the plugins directory; also the configuration key.
    pub file_name: String,
    /// Unique identity for routing and toggling: `file_name` without `.lua`.
    pub name: String,
    /// What the plugin says about itself, for display.
    pub info: PluginInfo,
    pub enabled: bool,
    pub error: Option<String>,
    lua: Option<Lua>,
    get_items_fn: Option<Function>,
    run_fn: Option<Function>,
    run_sub_fn: Option<Function>,
}

impl Plugin {
    pub(crate) fn load(path: &Path, relative: &str, info: PluginInfo) -> anyhow::Result<Self> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read plugin {}", path.display()))?;

        let lua = Lua::new();
        // Harden before the chunk runs so even the plugin's top-level code is
        // covered by the execution budget.
        lua_budget::disable_jit(&lua);
        lua_budget::with_budget(&lua, || lua.load(&source).exec())
            .with_context(|| format!("failed to initialize plugin {}", path.display()))?;

        let get_items_fn = lua.globals().get::<Option<Function>>("get_items")?;
        let run_fn = lua.globals().get::<Option<Function>>("run")?;
        let run_sub_fn = lua.globals().get::<Option<Function>>("run_sub")?;
        if get_items_fn.is_none() || run_fn.is_none() {
            anyhow::bail!(
                "plugin must define global functions `get_items(query)` and `run(value, query)`"
            );
        }
        if run_sub_fn.is_none() {
            anyhow::bail!("plugin must define global function `run_sub(value, sub_query)`");
        }

        Ok(Self {
            file_name: relative.to_string(),
            name: plugin_identity(relative),
            info,
            enabled: true,
            error: None,
            lua: Some(lua),
            get_items_fn,
            run_fn,
            run_sub_fn,
        })
    }

    /// Metadata-only entry for a plugin that has no live runtime — either
    /// disabled by the user or failed to load.
    fn unloaded(file_name: String, info: PluginInfo, enabled: bool) -> Self {
        Self {
            name: plugin_identity(&file_name),
            file_name,
            info,
            enabled,
            error: None,
            lua: None,
            get_items_fn: None,
            run_fn: None,
            run_sub_fn: None,
        }
    }

    /// Name to show in the UI: what the plugin calls itself, else the file stem.
    pub fn display_name(&self) -> &str {
        if self.info.name.is_empty() {
            &self.name
        } else {
            &self.info.name
        }
    }

    /// Whether this plugin is enabled and has a live runtime.
    pub fn available(&self) -> bool {
        self.enabled && self.lua.is_some() && self.get_items_fn.is_some()
    }

    /// Whether a Lua runtime is currently loaded for this plugin, regardless
    /// of whether the user has it enabled. A disabled plugin keeps its row in
    /// the manager (metadata only) without a runtime.
    pub fn runtime_loaded(&self) -> bool {
        self.lua.is_some()
    }

    pub fn query(&mut self, query: &str) -> Vec<PluginItem> {
        let Some(lua) = self.lua.as_ref() else {
            return Vec::new();
        };
        let Some(get_items) = self.get_items_fn.clone() else {
            return Vec::new();
        };
        match lua_budget::with_budget(lua, || {
            let table: Table = get_items.call(query)?;
            let mut items = Vec::new();
            for entry in table.sequence_values::<Table>() {
                let entry = entry?;
                let title: String = entry.get("title").unwrap_or_default();
                if title.is_empty() {
                    continue;
                }
                let value: String = match entry.get::<Option<String>>("value")? {
                    Some(v) => v,
                    None => title.clone(),
                };
                let sub: bool = entry.get::<Option<bool>>("sub")?.unwrap_or(false);
                items.push(PluginItem { title, value, sub });
            }
            Ok(items)
        }) {
            Ok(items) => items,
            Err(e) => {
                eprintln!("[plugin:{}] query error: {e}", self.name);
                self.error = Some(format!("get_items 失败: {e}"));
                Vec::new()
            }
        }
    }

    pub fn run(&mut self, value: &str, query: &str) {
        let Some(run_fn) = self.run_fn.clone() else {
            return;
        };
        self.run_impl("run", run_fn, value, query);
    }

    pub fn run_sub(&mut self, value: &str, sub_query: &str) {
        let Some(run_sub_fn) = self.run_sub_fn.clone() else {
            return;
        };
        self.run_impl("run_sub", run_sub_fn, value, sub_query);
    }

    fn run_impl(&mut self, handler: &str, run_fn: Function, value: &str, query: &str) {
        let Some(lua) = self.lua.as_ref() else {
            return;
        };
        let result = lua_budget::with_budget(lua, || {
            run_fn.call::<()>((value.to_string(), query.to_string()))
        });
        if let Err(e) = result {
            eprintln!("[plugin:{}] {handler} error: {e}", self.name);
            self.error = Some(format!("{handler} 失败: {e}"));
        }
    }

    pub fn unload(&mut self) {
        self.lua = None;
        self.get_items_fn = None;
        self.run_fn = None;
        self.run_sub_fn = None;
    }
}

/// A plugin file that is on disk but not loaded, because this build is outside
/// the range it supports. Kept so the control panel can show it and offer to
/// delete it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyPlugin {
    /// Path relative to the plugins directory, e.g. `legacy/author/thing-1.0.0.lua`.
    pub file_name: String,
    pub info: PluginInfo,
    pub reason: String,
}

pub struct PluginManager {
    pub plugins: Vec<Plugin>,
    /// Plugins parked in `legacy/`.
    pub legacy: Vec<LegacyPlugin>,
}

impl PluginManager {
    /// Load or reload every plugin from the plugins directory, parking the ones
    /// this build cannot run. Enabled state is taken from config (defaults to
    /// enabled).
    pub fn load_all(&mut self) {
        let Some(dir) = plugins_dir() else {
            eprintln!("[plugins] no data directory available; plugin loading disabled");
            return;
        };
        self.load_from(&dir);
    }

    fn load_from(&mut self, dir: &Path) {
        self.plugins.clear();
        self.legacy.clear();
        let config = Config::load();

        for relative in discover(dir, true) {
            let info = read_info(dir, &relative);
            if supports(&info, APP_VERSION) {
                let enabled = *config.plugins.get(&relative).unwrap_or(&true);
                Self::load_file(&mut self.plugins, dir, &relative, info, enabled);
            } else {
                let parked = format!("{LEGACY_DIR}/{relative}");
                match move_file(dir, &relative, &parked) {
                    // The legacy scan below lists it, so it is only parked here.
                    Ok(()) => eprintln!("[plugin] {relative} 不兼容，已移入 {parked}"),
                    Err(e) => {
                        eprintln!("[plugin] 无法移动 {relative} 到 legacy: {e:#}");
                        // Still visible, so the user can delete it by hand.
                        let reason = format!(
                            "{}（且无法移入 legacy: {e}）",
                            range_reason(&info, APP_VERSION)
                        );
                        self.legacy.push(LegacyPlugin {
                            file_name: relative,
                            info,
                            reason,
                        });
                    }
                }
            }
        }

        // A parked plugin comes back on its own once the range matches again,
        // so upgrading away from a version and back does not lose it.
        for relative in discover(&dir.join(LEGACY_DIR), false) {
            let parked = format!("{LEGACY_DIR}/{relative}");
            let info = read_info(dir, &parked);
            if !supports(&info, APP_VERSION) {
                let reason = range_reason(&info, APP_VERSION);
                self.legacy.push(LegacyPlugin {
                    file_name: parked,
                    info,
                    reason,
                });
                continue;
            }
            if dir.join(&relative).exists() {
                self.legacy.push(LegacyPlugin {
                    file_name: parked,
                    info,
                    reason: "已存在同名文件，未自动恢复".to_string(),
                });
                continue;
            }
            match move_file(dir, &parked, &relative) {
                Ok(()) => {
                    eprintln!("[plugin] {relative} 的版本区间重新匹配，已恢复");
                    let enabled = *config.plugins.get(&relative).unwrap_or(&true);
                    Self::load_file(&mut self.plugins, dir, &relative, info, enabled);
                }
                Err(e) => self.legacy.push(LegacyPlugin {
                    file_name: parked,
                    info,
                    reason: format!("无法移回原位置: {e}"),
                }),
            }
        }

        self.plugins.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        self.legacy.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    }

    /// Load one plugin file. A plugin that the user disabled is registered as
    /// metadata only: its chunk is never executed, so a file with side effects —
    /// or one disabled because it misbehaves — cannot run on startup.
    /// `set_enabled` performs the actual load when the user turns it back on.
    fn load_file(
        plugins: &mut Vec<Plugin>,
        dir: &Path,
        relative: &str,
        info: PluginInfo,
        enabled: bool,
    ) {
        if !enabled {
            plugins.push(Plugin::unloaded(relative.to_string(), info, false));
            return;
        }
        let path = dir.join(relative);
        match Plugin::load(&path, relative, info.clone()) {
            Ok(mut plugin) => {
                plugin.enabled = true;
                plugins.push(plugin);
            }
            Err(e) => {
                eprintln!("[plugin] failed to load {relative}: {e:#}");
                let mut plugin = Plugin::unloaded(relative.to_string(), info, true);
                plugin.error = Some(format!("{e:#}"));
                plugins.push(plugin);
            }
        }
    }

    /// Load a plugin file that just appeared, leaving the others alone.
    pub fn load_installed(&mut self, file_name: &str) -> anyhow::Result<()> {
        let dir = plugins_dir().context("no data directory available")?;
        self.load_installed_from(&dir, file_name)
    }

    fn load_installed_from(&mut self, dir: &Path, file_name: &str) -> anyhow::Result<()> {
        let enabled = *Config::load().plugins.get(file_name).unwrap_or(&true);
        let info = read_info(dir, file_name);
        self.plugins.retain(|plugin| plugin.file_name != file_name);
        Self::load_file(&mut self.plugins, dir, file_name, info, enabled);
        self.plugins.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        Ok(())
    }

    /// Enabled plugins that the freshly installed one replaces: the same plugin
    /// by declared author and name, or — for a file still sitting in the flat
    /// layout, where there is no `<author>/` directory to tell them apart — the
    /// same script by file name. That is how an early `calc.lua` is recognised
    /// as today's `hemingtsai/计算器-1.0.0.lua`.
    pub fn superseded_by(&self, installed: &Installed) -> Vec<String> {
        let key = plugin_key(&installed.info);
        let source_stem = installed.source_stem.to_lowercase();
        self.plugins
            .iter()
            .filter(|plugin| plugin.file_name != installed.file_name && plugin.enabled)
            .filter(|plugin| {
                let same_plugin = plugin_key(&plugin.info) == key;
                let flat = !plugin.file_name.contains('/');
                let same_script = flat
                    && !source_stem.is_empty()
                    && stem_without_version(&plugin.file_name).to_lowercase() == source_stem;
                same_plugin || same_script
            })
            .map(|plugin| plugin.file_name.clone())
            .collect()
    }

    /// Register a freshly installed script and switch off every other version
    /// of the same plugin, so one plugin means one active version instead of
    /// two identical rows. Returns the identities that were switched off.
    pub fn adopt_installed(&mut self, installed: &Installed) -> Vec<String> {
        let superseded = self.superseded_by(installed);
        for relative in &superseded {
            if let Err(e) = crate::config::modify(|config| {
                config.plugins.insert(relative.clone(), false);
            }) {
                eprintln!("[plugin] failed to disable {relative}: {e:#}");
            }
            if let Some(plugin) = self.find_by_file_mut(relative) {
                plugin.enabled = false;
                plugin.unload();
            }
        }
        superseded
    }

    /// Delete a plugin file — active or parked in `legacy/` — and forget its
    /// enabled state. Directories it leaves empty are removed.
    pub fn delete(&mut self, file_name: &str) -> anyhow::Result<()> {
        let dir = plugins_dir().context("no data directory available")?;
        self.delete_in(&dir, file_name)
    }

    fn delete_in(&mut self, dir: &Path, file_name: &str) -> anyhow::Result<()> {
        remove_file(dir, file_name)?;
        crate::config::modify(|config| {
            config.plugins.remove(file_name);
        })?;
        self.plugins.retain(|plugin| plugin.file_name != file_name);
        self.legacy.retain(|plugin| plugin.file_name != file_name);
        Ok(())
    }

    pub fn find_by_file_mut(&mut self, file_name: &str) -> Option<&mut Plugin> {
        self.plugins
            .iter_mut()
            .find(|plugin| plugin.file_name == file_name)
    }

    pub fn find_by_name_mut(&mut self, name: &str) -> Option<&mut Plugin> {
        self.plugins.iter_mut().find(|plugin| plugin.name == name)
    }

    /// Enable/disable a plugin by its relative path; persists to config and
    /// loads/unloads its runtime accordingly.
    pub fn set_enabled(&mut self, file_name: &str, enabled: bool) -> anyhow::Result<()> {
        crate::config::modify(|config| {
            config.plugins.insert(file_name.to_string(), enabled);
        })?;

        let Some(plugin) = self.find_by_file_mut(file_name) else {
            return Ok(());
        };
        plugin.enabled = enabled;
        if enabled && plugin.lua.is_none() {
            let Some(dir) = plugins_dir() else {
                anyhow::bail!("no data directory available");
            };
            let info = read_info(&dir, file_name);
            match Plugin::load(&dir.join(file_name), file_name, info.clone()) {
                Ok(mut loaded) => {
                    loaded.enabled = true;
                    *plugin = loaded;
                }
                Err(e) => {
                    plugin.error = Some(format!("{e:#}"));
                    return Err(e);
                }
            }
        } else if !enabled {
            plugin.unload();
        }
        Ok(())
    }
}

/// Metadata of a plugin file, read from its source (never executed).
fn read_info(dir: &Path, relative: &str) -> PluginInfo {
    let stem = Path::new(relative)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_default();
    match std::fs::read_to_string(dir.join(relative)) {
        Ok(source) => scan_metadata(&source, &stem),
        Err(_) => PluginInfo::with_name(&stem),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNS: &str = "function get_items(query) return {} end\nfunction run(value, query) end\nfunction run_sub(value, sub_query) end\n";

    /// Plugin body that marks a file when its chunk runs.
    fn marker_plugin(marker: &std::path::Path, extra: &str) -> String {
        format!(
            r##"
            local f = assert(io.open("{marker}", "w"))
            f:write("ran")
            f:close()
            {extra}
            {RUNS}
            "##,
            marker = marker.display()
        )
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---------------------------------------------------------------- metadata

    #[test]
    fn metadata_is_read_from_a_literal_table() {
        let info = scan_metadata(
            r##"
            -- 计算器插件
            PLUGIN = {
                name = "计算器",
                version = "1.0.0",
                author = "hemingtsai",
                license = "MIT",
                repository = "https://github.com/hemingtsai/touchery",
                description = "四则运算加次方",
                min_touchery = "1.3.0",
                max_touchery = "2.0.0",
            }
            function get_items(query) return {} end
            "##,
            "calc",
        );
        assert_eq!(info.name, "计算器");
        assert_eq!(info.version.as_deref(), Some("1.0.0"));
        assert_eq!(info.author.as_deref(), Some("hemingtsai"));
        assert_eq!(info.license.as_deref(), Some("MIT"));
        assert_eq!(
            info.repository.as_deref(),
            Some("https://github.com/hemingtsai/touchery")
        );
        assert_eq!(info.description.as_deref(), Some("四则运算加次方"));
        assert_eq!(info.min_touchery.as_deref(), Some("1.3.0"));
        assert_eq!(info.max_touchery.as_deref(), Some("2.0.0"));
        assert_eq!(range_text(&info).as_deref(), Some("1.3.0 – 2.0.0"));

        // No table at all: the file stem is the name and the range is open.
        let bare = scan_metadata(RUNS, "plain");
        assert_eq!(bare.name, "plain");
        assert_eq!(bare.version, None);
        assert!(supports(&bare, APP_VERSION));
    }

    #[test]
    fn the_scanner_ignores_anything_that_is_not_a_literal() {
        // Single quotes, one line, trailing comments, and braces inside strings.
        let info = scan_metadata(
            r#"
            PLUGIN = { name = '计算器', version = "1.0.0", -- 版本
                       description = "}" .. "{", }
            "#,
            "stem",
        );
        assert_eq!(info.name, "计算器");
        assert_eq!(info.version.as_deref(), Some("1.0.0"));
        assert_eq!(info.description, None, "a concatenation is not a literal");

        // Computed values are ignored, so the field reads as not declared.
        let computed = scan_metadata(
            r#"
            local base = "1.0"
            PLUGIN = { name = "x", version = base .. ".0", license = tostring(2) }
            "#,
            "stem",
        );
        assert_eq!(computed.version, None);
        assert_eq!(computed.license, None);
        assert_eq!(computed.name, "x");

        // A mention inside a comment or a string is not a declaration.
        let commented = scan_metadata("-- PLUGIN = { name = \"注释里的\" }\n", "stem");
        assert_eq!(commented.name, "stem");
        let in_string = scan_metadata("print(\"PLUGIN = { name = '字符串里的' }\")\n", "stem");
        assert_eq!(in_string.name, "stem");

        // Nested tables do not leak their fields into the metadata.
        let nested = scan_metadata(
            "PLUGIN = { name = \"top\", touchery = { min = \"9.9.9\" } }",
            "stem",
        );
        assert_eq!(nested.name, "top");
        assert_eq!(nested.min_touchery, None, "only top-level fields count");

        // Empty and oversized values, and a non-http repository.
        let junk = scan_metadata(
            r#"
            PLUGIN = {
                name = "",
                author = "   ",
                repository = "file:///etc/passwd",
                description = string.rep("x", 500),
                min_touchery = "abc",
            }
            "#,
            "stem",
        );
        assert_eq!(junk.name, "stem", "an empty name falls back to the stem");
        assert_eq!(junk.author, None);
        assert_eq!(junk.repository, None, "only http(s) survives");
        assert_eq!(junk.description, None);
        assert_eq!(junk.min_touchery, None, "an unparseable bound is dropped");

        // A long literal is capped rather than rejected.
        let long = format!("PLUGIN = {{ description = \"{}\" }}", "y".repeat(500));
        assert_eq!(
            scan_metadata(&long, "stem")
                .description
                .unwrap()
                .chars()
                .count(),
            INFO_MAX
        );
    }

    // ---------------------------------------------------------------- versions

    #[test]
    fn version_ranges_are_compared_numerically() {
        assert_eq!(version_tuple("1.3.0"), Some((1, 3, 0)));
        assert_eq!(version_tuple("1.3"), Some((1, 3, 0)));
        assert_eq!(version_tuple("2"), Some((2, 0, 0)));
        assert_eq!(version_tuple("v1.3.0"), Some((1, 3, 0)));
        assert_eq!(version_tuple(" 1.3.0-beta "), Some((1, 3, 0)));
        assert_eq!(version_tuple("1.3.0+build7"), Some((1, 3, 0)));
        assert_eq!(version_tuple("abc"), None);

        let range = |min: Option<&str>, max: Option<&str>| PluginInfo {
            min_touchery: min.map(str::to_string),
            max_touchery: max.map(str::to_string),
            ..PluginInfo::with_name("x")
        };

        assert!(supports(&range(None, None), "1.3.0-beta"));
        assert!(supports(&range(Some("1.3.0"), None), "1.3.0-beta"));
        assert!(!supports(&range(Some("1.4.0"), None), "1.3.0-beta"));
        assert!(supports(&range(None, Some("1.3.0")), "1.3.0-beta"));
        assert!(!supports(&range(None, Some("1.2.9")), "1.3.0-beta"));
        assert!(supports(&range(Some("1.0.0"), Some("2.0.0")), "1.3.0-beta"));
        // Bounds are inclusive.
        assert!(supports(&range(Some("1.3.0"), Some("1.3.0")), "1.3.0"));
        assert!(supports(&range(Some("1.0"), None), "1.0.0"));
        assert!(!supports(&range(Some("1.0.1"), None), "1.0.0"));
    }

    // ---------------------------------------------------------------- layout

    #[test]
    fn a_script_is_installed_under_its_author_and_version() {
        let dir = temp_dir("touchery-layout-test");
        let plugins = dir.join("plugins");

        let source = dir.join("calc.lua");
        std::fs::write(
            &source,
            format!(
                "PLUGIN = {{ name = \"计算器\", version = \"1.0.0\", author = \"hemingtsai\" }}\n{RUNS}"
            ),
        )
        .unwrap();
        let installed = install_into(&plugins, &source).unwrap();
        assert_eq!(installed.file_name, "hemingtsai/计算器-1.0.0.lua");
        assert!(!installed.replaced);
        assert_eq!(installed.info.name, "计算器");
        assert!(plugins.join("hemingtsai/计算器-1.0.0.lua").exists());

        // Reinstalling the same version replaces it instead of piling up.
        let again = install_into(&plugins, &source).unwrap();
        assert_eq!(again.file_name, "hemingtsai/计算器-1.0.0.lua");
        assert!(again.replaced);

        // A different version lives beside it.
        std::fs::write(
            &source,
            format!(
                "PLUGIN = {{ name = \"计算器\", version = \"1.1.0\", author = \"hemingtsai\" }}\n{RUNS}"
            ),
        )
        .unwrap();
        let newer = install_into(&plugins, &source).unwrap();
        assert_eq!(newer.file_name, "hemingtsai/计算器-1.1.0.lua");
        assert!(plugins.join("hemingtsai/计算器-1.0.0.lua").exists());

        // No metadata: the source file name, under `unknown/`.
        let plain = dir.join("plain.lua");
        std::fs::write(&plain, RUNS).unwrap();
        let installed = install_into(&plugins, &plain).unwrap();
        assert_eq!(installed.file_name, "unknown/plain.lua");

        // Metadata that would escape the directory is neutralised.
        let evil = dir.join("evil.lua");
        std::fs::write(
            &evil,
            format!(
                "PLUGIN = {{ name = \"../evil\", version = \"1/2\", author = \"..\" }}\n{RUNS}"
            ),
        )
        .unwrap();
        let installed = install_into(&plugins, &evil).unwrap();
        // "../evil" loses its dots and stays one component; an author of ".."
        // is meaningless and falls back to `unknown`.
        assert_eq!(installed.file_name, "unknown/-evil-1-2.lua");
        assert!(plugins.join(&installed.file_name).exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installing_refuses_what_is_not_a_lua_script() {
        let dir = temp_dir("touchery-install-refuse-test");
        let plugins = dir.join("plugins");

        let not_lua = dir.join("script.txt");
        std::fs::write(&not_lua, RUNS).unwrap();
        assert!(install_into(&plugins, &not_lua).is_err());
        assert!(!plugins.exists(), "nothing may be written on refusal");

        let empty = dir.join("empty.lua");
        std::fs::write(&empty, "   \n").unwrap();
        assert!(install_into(&plugins, &empty).is_err());

        let binary = dir.join("binary.lua");
        std::fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
        assert!(install_into(&plugins, &binary).is_err());

        let missing = dir.join("missing.lua");
        assert!(install_into(&plugins, &missing).is_err());

        // A directory that merely ends in .lua is not a script either.
        let fake = dir.join("folder.lua");
        std::fs::create_dir_all(&fake).unwrap();
        assert!(install_into(&plugins, &fake).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------------------------------------------------------- loading

    #[test]
    fn disabled_plugins_are_not_executed_at_startup() {
        let dir = temp_dir("touchery-plugin-disabled-test");
        let marker = dir.join("marker.txt");
        let plugin_path = dir.join("probe.lua");
        std::fs::write(&plugin_path, marker_plugin(&marker, "")).unwrap();
        let info = PluginInfo::with_name("probe");

        let mut plugins = Vec::new();
        PluginManager::load_file(&mut plugins, &dir, "probe.lua", info.clone(), false);
        assert!(!marker.exists(), "a disabled plugin must not run its chunk");
        assert_eq!(plugins.len(), 1);
        assert!(!plugins[0].enabled);
        assert!(!plugins[0].runtime_loaded());

        PluginManager::load_file(&mut plugins, &dir, "probe.lua", info, true);
        assert!(marker.exists(), "an enabled plugin must be loaded");
        assert!(plugins[1].enabled);
        assert!(plugins[1].runtime_loaded());
        assert_eq!(plugins[1].name, "probe");
    }

    #[test]
    fn author_folders_are_discovered_and_legacy_is_skipped() {
        let dir = temp_dir("touchery-discovery-test");
        std::fs::create_dir_all(dir.join("hemingtsai")).unwrap();
        std::fs::create_dir_all(dir.join(LEGACY_DIR).join("hemingtsai")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join("top.lua"), RUNS).unwrap();
        std::fs::write(dir.join("hemingtsai/calc-1.0.0.lua"), RUNS).unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        std::fs::write(dir.join(LEGACY_DIR).join("old.lua"), RUNS).unwrap();
        std::fs::write(dir.join(LEGACY_DIR).join("hemingtsai/parked.lua"), RUNS).unwrap();
        std::fs::write(dir.join(".hidden/secret.lua"), RUNS).unwrap();

        assert_eq!(
            discover(&dir, true),
            vec![
                "hemingtsai/calc-1.0.0.lua".to_string(),
                "top.lua".to_string()
            ]
        );
        assert_eq!(
            discover(&dir.join(LEGACY_DIR), false),
            vec!["hemingtsai/parked.lua".to_string(), "old.lua".to_string()]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_installed_plugin_is_loaded_without_touching_the_others() {
        let dir = temp_dir("touchery-install-load-test");
        let plugins = dir.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();

        let mut manager = PluginManager {
            plugins: Vec::new(),
            legacy: Vec::new(),
        };

        std::fs::write(plugins.join("existing.lua"), RUNS).unwrap();
        manager
            .load_installed_from(&plugins, "existing.lua")
            .unwrap();
        assert!(manager.plugins[0].runtime_loaded());

        // A broken script lands as a row with an error instead of vanishing.
        std::fs::write(plugins.join("broken.lua"), "this is not lua").unwrap();
        manager.load_installed_from(&plugins, "broken.lua").unwrap();
        assert_eq!(manager.plugins.len(), 2);
        let row = manager
            .plugins
            .iter()
            .find(|plugin| plugin.file_name == "broken.lua")
            .expect("the failed plugin keeps its row");
        assert!(row.error.is_some(), "the failure must be visible");
        assert!(!row.runtime_loaded());
        assert!(
            manager
                .plugins
                .iter()
                .find(|plugin| plugin.file_name == "existing.lua")
                .unwrap()
                .runtime_loaded(),
            "the working plugin was not reloaded"
        );

        // Reinstalling the same name replaces the row instead of duplicating it.
        std::fs::write(plugins.join("broken.lua"), RUNS).unwrap();
        manager.load_installed_from(&plugins, "broken.lua").unwrap();
        assert_eq!(manager.plugins.len(), 2);
        assert!(
            manager
                .plugins
                .iter()
                .find(|plugin| plugin.file_name == "broken.lua")
                .unwrap()
                .error
                .is_none()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------------------------------------------------------- legacy

    #[test]
    fn incompatible_plugins_are_parked_and_come_back_when_supported() {
        let dir = temp_dir("touchery-legacy-test");
        let marker = dir.join("marker.txt");
        std::fs::create_dir_all(dir.join("author")).unwrap();
        // Declares a range this build is outside: it must be parked, and its
        // chunk must never run.
        std::fs::write(
            dir.join("author/tool-1.0.0.lua"),
            marker_plugin(&marker, "PLUGIN = { name = \"tool\", version = \"1.0.0\", author = \"author\", max_touchery = \"0.9.0\" }"),
        )
        .unwrap();
        // Compatible, declared as a literal.
        std::fs::write(
            dir.join("author/kept.lua"),
            format!("PLUGIN = {{ name = \"kept\", author = \"author\" }}\n{RUNS}"),
        )
        .unwrap();

        let mut manager = PluginManager {
            plugins: Vec::new(),
            legacy: Vec::new(),
        };
        manager.load_from(&dir);

        assert_eq!(
            manager.plugins.len(),
            1,
            "{:?}",
            manager
                .plugins
                .iter()
                .map(|p| &p.file_name)
                .collect::<Vec<_>>()
        );
        assert_eq!(manager.plugins[0].file_name, "author/kept.lua");
        assert!(!marker.exists(), "a parked plugin must not be executed");
        assert!(!dir.join("author/tool-1.0.0.lua").exists());
        assert!(dir.join(LEGACY_DIR).join("author/tool-1.0.0.lua").exists());
        assert_eq!(manager.legacy.len(), 1);
        assert_eq!(manager.legacy[0].file_name, "legacy/author/tool-1.0.0.lua");
        assert!(
            manager.legacy[0].reason.contains("0.9.0"),
            "{}",
            manager.legacy[0].reason
        );

        // Widen the range in place: the next scan brings it back and runs it.
        std::fs::write(
            dir.join(LEGACY_DIR).join("author/tool-1.0.0.lua"),
            marker_plugin(&marker, "PLUGIN = { name = \"tool\", version = \"1.0.0\", author = \"author\", min_touchery = \"1.0.0\" }"),
        )
        .unwrap();
        manager.load_from(&dir);
        assert!(manager.legacy.is_empty(), "{:?}", manager.legacy);
        assert_eq!(manager.plugins.len(), 2);
        assert!(dir.join("author/tool-1.0.0.lua").exists());
        assert!(marker.exists(), "a supported plugin is loaded and runs");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------------------------------------------------------- supersede

    #[test]
    fn a_newer_version_switches_the_older_one_off() {
        let info = |version: &str| PluginInfo {
            version: Some(version.to_string()),
            author: Some("hemingtsai".to_string()),
            ..PluginInfo::with_name("计算器")
        };
        let manager = PluginManager {
            plugins: vec![
                Plugin::unloaded("hemingtsai/计算器-1.0.0.lua".into(), info("1.0.0"), true),
                Plugin::unloaded("hemingtsai/计算器-0.9.0.lua".into(), info("0.9.0"), false),
                // Another author's plugin of the same name is not the same plugin.
                Plugin::unloaded(
                    "someone/计算器-1.0.0.lua".into(),
                    PluginInfo {
                        author: Some("someone".to_string()),
                        ..info("1.0.0")
                    },
                    true,
                ),
                Plugin::unloaded(
                    "hemingtsai/别的-1.0.0.lua".into(),
                    PluginInfo {
                        author: Some("hemingtsai".to_string()),
                        ..PluginInfo::with_name("别的")
                    },
                    true,
                ),
            ],
            legacy: Vec::new(),
        };
        let installed = Installed {
            file_name: "hemingtsai/计算器-1.1.0.lua".to_string(),
            info: info("1.1.0"),
            replaced: false,
            source_stem: "计算器".to_string(),
        };
        assert_eq!(
            manager.superseded_by(&installed),
            vec!["hemingtsai/计算器-1.0.0.lua".to_string()],
            "only the enabled older version of the same plugin is switched off"
        );

        // An earlier flat install of the same script (no metadata at all) is
        // recognised by its file name, which is the state a user upgrading from
        // the old layout is in.
        let manager = PluginManager {
            plugins: vec![
                Plugin::unloaded("calc.lua".into(), PluginInfo::with_name("calc"), true),
                Plugin::unloaded(
                    "hemingtsai/别的-1.0.0.lua".into(),
                    PluginInfo {
                        author: Some("hemingtsai".to_string()),
                        ..PluginInfo::with_name("别的")
                    },
                    true,
                ),
            ],
            legacy: Vec::new(),
        };
        let installed = Installed {
            file_name: "hemingtsai/计算器-1.0.0.lua".to_string(),
            info: PluginInfo {
                author: Some("hemingtsai".to_string()),
                version: Some("1.0.0".to_string()),
                ..PluginInfo::with_name("计算器")
            },
            replaced: false,
            source_stem: "calc".to_string(),
        };
        assert_eq!(
            manager.superseded_by(&installed),
            vec!["calc.lua".to_string()],
            "the same script under the old layout is switched off"
        );

        // Version suffixes are stripped before comparing stems.
        assert_eq!(stem_without_version("a/计算器-1.0.0.lua"), "计算器");
        assert_eq!(stem_without_version("a/计算器-1.0.0-beta.lua"), "计算器");
        assert_eq!(stem_without_version("calc.lua"), "calc");
        assert_eq!(stem_without_version("my-plugin.lua"), "my-plugin");
    }

    // ---------------------------------------------------------------- delete

    #[test]
    fn deleting_a_plugin_removes_the_file_and_its_empty_folder() {
        let dir = temp_dir("touchery-delete-test");
        std::fs::create_dir_all(dir.join("hemingtsai")).unwrap();
        std::fs::write(dir.join("hemingtsai/calc-1.0.0.lua"), RUNS).unwrap();
        std::fs::create_dir_all(dir.join(LEGACY_DIR).join("someone")).unwrap();
        std::fs::write(dir.join(LEGACY_DIR).join("someone/old.lua"), RUNS).unwrap();

        remove_file(&dir, "hemingtsai/calc-1.0.0.lua").unwrap();
        assert!(
            !dir.join("hemingtsai").exists(),
            "the empty folder goes too"
        );

        remove_file(&dir, "legacy/someone/old.lua").unwrap();
        assert!(!dir.join(LEGACY_DIR).exists());

        // The plugins directory itself is never removed.
        std::fs::write(dir.join("top.lua"), RUNS).unwrap();
        remove_file(&dir, "top.lua").unwrap();
        assert!(dir.exists());

        // A path that would escape the directory is refused.
        assert!(remove_file(&dir, "../../etc/hosts").is_err());
        assert!(remove_file(&dir, "/etc/hosts").is_err());
        assert!(remove_file(&dir, "hemingtsai/../../outside.lua").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
