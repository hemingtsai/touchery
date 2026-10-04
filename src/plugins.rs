use crate::config::Config;
use crate::lua_budget;
use anyhow::Context as _;
use mlua::{Function, Lua, Table, Value};
use std::path::PathBuf;

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
///   name = "计算器", version = "1.0.0", author = "…",
///   license = "MIT", repository = "https://…", description = "…",
/// }
/// ```
///
/// Every field is optional; `name` falls back to the file stem. Values are
/// trimmed and length-capped, and `repository` is only kept when it is an
/// http(s) URL — it is handed to `open`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginInfo {
    pub name: String,
    pub version: Option<String>,
    pub author: Option<String>,
    pub license: Option<String>,
    pub repository: Option<String>,
    pub description: Option<String>,
}

impl PluginInfo {
    fn with_name(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Self::default()
        }
    }
}

/// Longest accepted metadata string, per field.
const INFO_MAX: usize = 120;

/// Read the `PLUGIN` table a plugin may define. A malformed or missing table
/// simply means "no metadata"; it never fails the load.
fn read_info(lua: &Lua, fallback_name: &str) -> PluginInfo {
    let mut info = PluginInfo::with_name(fallback_name);
    let Ok(Some(table)) = lua.globals().get::<Option<Table>>("PLUGIN") else {
        return info;
    };
    // Only real Lua strings count: a malformed table must not smuggle
    // anything else in (numbers would otherwise be coerced for us).
    let field = |key: &str| -> Option<String> {
        match table.get::<Option<Value>>(key).ok().flatten() {
            Some(Value::String(value)) => {
                let value = value.to_string_lossy();
                let value = value.trim();
                let value: String = value.chars().take(INFO_MAX).collect();
                (!value.is_empty()).then_some(value)
            }
            _ => None,
        }
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
    info
}

pub struct Plugin {
    pub file_name: String,
    /// Unique identity of the plugin: the file stem. Routing and toggling use
    /// this, never the declared name, which two files may share.
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
    pub(crate) fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let file_name = path
            .file_name()
            .context("no file name")?
            .to_string_lossy()
            .to_string();
        let name = path
            .file_stem()
            .context("no file stem")?
            .to_string_lossy()
            .to_string();

        let source = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read plugin {}", path.display()))?;

        let lua = Lua::new();
        // Harden before the chunk runs so even the plugin's top-level code is
        // covered by the execution budget.
        lua_budget::disable_jit(&lua);
        lua_budget::with_budget(&lua, || lua.load(&source).exec())
            .with_context(|| format!("failed to initialize plugin {}", path.display()))?;

        let info = read_info(&lua, &name);
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
            file_name,
            name,
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
    fn unloaded(file_name: String, name: String, enabled: bool) -> Self {
        let info = PluginInfo::with_name(&name);
        Self {
            file_name,
            name,
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

/// Directory containing user plugins. Returns `None` when no data directory
/// can be determined — plugin loading is then disabled entirely (never falls
/// back to the current working directory, which would read arbitrary code
/// relative to wherever the binary was launched).
pub fn plugins_dir() -> Option<PathBuf> {
    crate::config::data_root().map(|root| root.join("plugins"))
}

/// Install a Lua script into the plugins directory.
///
/// The script is validated (UTF-8, non-empty, `.lua`), copied byte for byte and
/// never overwrites an existing plugin: a name that is taken gets the next free
/// `name-2.lua`, so an installed plugin — and any edits the user made to it —
/// survive. Returns the file name it was installed as.
pub fn install_script(source: &std::path::Path) -> anyhow::Result<String> {
    let dir = plugins_dir().context("no data directory available")?;
    install_into(&dir, source)
}

fn install_into(dir: &std::path::Path, source: &std::path::Path) -> anyhow::Result<String> {
    let file_name = source
        .file_name()
        .context("no file name")?
        .to_string_lossy()
        .to_string();
    if !file_name.to_ascii_lowercase().ends_with(".lua") {
        anyhow::bail!("只能安装 .lua 文件（收到 {file_name}）");
    }
    // Read it first: a directory, a binary or a non-UTF-8 file must be refused
    // before anything lands in the plugins directory.
    let text = std::fs::read_to_string(source)
        .with_context(|| format!("无法读取 {}", source.display()))?;
    if text.trim().is_empty() {
        anyhow::bail!("{file_name} 是空文件");
    }

    std::fs::create_dir_all(dir).with_context(|| format!("无法创建 {}", dir.display()))?;
    let file_name = free_file_name(dir, &file_name);
    let target = dir.join(&file_name);
    // Copy the bytes rather than the decoded text so line endings and encoding
    // survive exactly.
    std::fs::copy(source, &target).with_context(|| format!("无法写入 {}", target.display()))?;
    Ok(file_name)
}

/// `calc.lua` -> `calc.lua`, then `calc-2.lua`, `calc-3.lua`, …
fn free_file_name(dir: &std::path::Path, file_name: &str) -> String {
    if !dir.join(file_name).exists() {
        return file_name.to_string();
    }
    let (stem, extension) = match file_name.rsplit_once('.') {
        Some((stem, extension)) => (stem, format!(".{extension}")),
        None => (file_name, String::new()),
    };
    for suffix in 2..1000 {
        let candidate = format!("{stem}-{suffix}{extension}");
        if !dir.join(&candidate).exists() {
            return candidate;
        }
    }
    format!("{stem}-{}{extension}", std::process::id())
}

pub struct PluginManager {
    pub plugins: Vec<Plugin>,
}

impl PluginManager {
    /// Load or reload every plugin from the plugins directory.
    /// Enabled state is taken from config (defaults to enabled).
    pub fn load_all(&mut self) {
        self.plugins.clear();
        let Some(dir) = plugins_dir() else {
            eprintln!("[plugins] no data directory available; plugin loading disabled");
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        let config = Config::load();

        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "lua"))
            .collect();
        files.sort();

        for path in files {
            let file_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let enabled = *config.plugins.get(&file_name).unwrap_or(&true);
            Self::load_file(&mut self.plugins, &path, enabled);
        }
    }

    /// Load one plugin file into `plugins`. A plugin that the user disabled is
    /// registered as metadata only: its chunk is never executed, so a file
    /// with side effects — or one disabled because it misbehaves — cannot run
    /// on startup. `set_enabled` performs the actual load when the user turns
    /// it back on.
    fn load_file(plugins: &mut Vec<Plugin>, path: &std::path::Path, enabled: bool) {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        if !enabled {
            plugins.push(Plugin::unloaded(file_name, name, false));
            return;
        }

        match Plugin::load(path) {
            Ok(mut plugin) => {
                plugin.enabled = true;
                plugins.push(plugin);
            }
            Err(e) => {
                eprintln!("[plugin] failed to load {}: {e:#}", path.display());
                let mut plugin = Plugin::unloaded(file_name, name, enabled);
                plugin.error = Some(format!("{e:#}"));
                plugins.push(plugin);
            }
        }
    }

    /// Load a plugin file that was just installed, leaving the runtimes of the
    /// others alone. A failure is recorded on the plugin's row (and returned),
    /// exactly like a failure at startup.
    pub fn load_installed(&mut self, file_name: &str) -> anyhow::Result<()> {
        let dir = plugins_dir().context("no data directory available")?;
        let enabled = *Config::load().plugins.get(file_name).unwrap_or(&true);
        self.load_installed_from(&dir, file_name, enabled);
        Ok(())
    }

    fn load_installed_from(&mut self, dir: &std::path::Path, file_name: &str, enabled: bool) {
        self.plugins.retain(|plugin| plugin.file_name != file_name);
        Self::load_file(&mut self.plugins, &dir.join(file_name), enabled);
        self.plugins.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    }

    /// Other plugins that look like the same one: same declared name
    /// (case-insensitive) or same repository. Used to warn when installing a
    /// second copy of a plugin.
    pub fn duplicates_of(&self, file_name: &str) -> Vec<String> {
        let Some(plugin) = self
            .plugins
            .iter()
            .find(|plugin| plugin.file_name == file_name)
        else {
            return Vec::new();
        };
        let name = plugin.info.name.trim().to_lowercase();
        let repository = plugin.info.repository.clone();
        self.plugins
            .iter()
            .filter(|other| other.file_name != file_name)
            .filter(|other| {
                let same_name = !name.is_empty() && other.info.name.trim().to_lowercase() == name;
                let same_repository = repository
                    .as_deref()
                    .is_some_and(|url| other.info.repository.as_deref() == Some(url));
                same_name || same_repository
            })
            .map(|other| other.file_name.clone())
            .collect()
    }

    pub fn find_by_file_mut(&mut self, file_name: &str) -> Option<&mut Plugin> {
        self.plugins.iter_mut().find(|p| p.file_name == file_name)
    }

    pub fn find_by_name_mut(&mut self, name: &str) -> Option<&mut Plugin> {
        self.plugins.iter_mut().find(|p| p.name == name)
    }

    /// Enable/disable a plugin by file name; persists to config and
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
            match Plugin::load(&dir.join(file_name)) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_probe_plugin(dir: &std::path::Path, marker: &std::path::Path) -> PathBuf {
        let plugin_path = dir.join("probe.lua");
        std::fs::write(
            &plugin_path,
            format!(
                r##"
                local f = assert(io.open("{marker}", "w"))
                f:write("ran")
                f:close()
                function get_items(query) return {{}} end
                function run(value, query) end
                function run_sub(value, sub_query) end
                "##,
                marker = marker.display()
            ),
        )
        .unwrap();
        plugin_path
    }

    #[test]
    fn installing_a_script_copies_it_into_the_plugins_directory() {
        let dir = std::env::temp_dir().join("touchery-install-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("calc.lua");
        std::fs::write(
            &source,
            "-- calculator\nfunction get_items(q) return {} end\n",
        )
        .unwrap();

        let plugins_dir = dir.join("plugins");
        let installed = install_into(&plugins_dir, &source).unwrap();
        assert_eq!(installed, "calc.lua");
        assert_eq!(
            std::fs::read_to_string(plugins_dir.join("calc.lua")).unwrap(),
            std::fs::read_to_string(&source).unwrap()
        );

        // A second install of the same name must not clobber the first one.
        let installed = install_into(&plugins_dir, &source).unwrap();
        assert_eq!(installed, "calc-2.lua");
        let installed = install_into(&plugins_dir, &source).unwrap();
        assert_eq!(installed, "calc-3.lua");
        assert!(plugins_dir.join("calc.lua").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn plugin_with_info(file_name: &str, info: PluginInfo) -> Plugin {
        let mut plugin =
            Plugin::unloaded(file_name.to_string(), file_name.replace(".lua", ""), true);
        plugin.info = info;
        plugin
    }

    #[test]
    fn a_plugin_can_describe_itself() {
        let dir = std::env::temp_dir().join("touchery-plugin-info-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Full metadata.
        let full = dir.join("calc.lua");
        std::fs::write(
            &full,
            r##"
            PLUGIN = {
                name = "计算器",
                version = "1.0.0",
                author = "hemingtsai",
                license = "MIT",
                repository = "https://github.com/hemingtsai/touchery",
                description = "四则运算加次方",
            }
            function get_items(query) return {} end
            function run(value, query) end
            function run_sub(value, sub_query) end
            "##,
        )
        .unwrap();
        let plugin = Plugin::load(&full).unwrap();
        assert_eq!(plugin.display_name(), "计算器");
        assert_eq!(plugin.info.version.as_deref(), Some("1.0.0"));
        assert_eq!(plugin.info.author.as_deref(), Some("hemingtsai"));
        assert_eq!(plugin.info.license.as_deref(), Some("MIT"));
        assert_eq!(
            plugin.info.repository.as_deref(),
            Some("https://github.com/hemingtsai/touchery")
        );
        assert_eq!(plugin.info.description.as_deref(), Some("四则运算加次方"));
        // Identity stays the file stem even when the plugin renames itself.
        assert_eq!(plugin.name, "calc");

        // No metadata at all: the file stem is the name.
        let bare = dir.join("plain.lua");
        std::fs::write(
            &bare,
            "function get_items(query) return {} end\nfunction run(value, query) end\nfunction run_sub(value, sub_query) end\n",
        )
        .unwrap();
        let plugin = Plugin::load(&bare).unwrap();
        assert_eq!(plugin.display_name(), "plain");
        assert!(plugin.info.version.is_none());

        // Garbage metadata is ignored rather than fatal, and a non-http
        // "repository" is dropped (it is handed to `open`).
        let junk = dir.join("junk.lua");
        std::fs::write(
            &junk,
            r##"
            PLUGIN = {
                name = 42,
                version = "  1.2  ",
                author = "",
                repository = "file:///etc/passwd",
                description = string.rep("x", 500),
            }
            function get_items(query) return {} end
            function run(value, query) end
            function run_sub(value, sub_query) end
            "##,
        )
        .unwrap();
        let plugin = Plugin::load(&junk).unwrap();
        assert_eq!(
            plugin.display_name(),
            "junk",
            "a non-string name is ignored"
        );
        assert_eq!(plugin.info.version.as_deref(), Some("1.2"), "trimmed");
        assert!(plugin.info.author.is_none(), "empty strings are dropped");
        assert!(plugin.info.repository.is_none(), "only http(s) survives");
        assert_eq!(plugin.info.description.unwrap().chars().count(), INFO_MAX);

        // A PLUGIN that is not a table must not break the load either.
        let wrong = dir.join("wrong.lua");
        std::fs::write(
            &wrong,
            "PLUGIN = \"nope\"\nfunction get_items(query) return {} end\nfunction run(value, query) end\nfunction run_sub(value, sub_query) end\n",
        )
        .unwrap();
        assert_eq!(Plugin::load(&wrong).unwrap().display_name(), "wrong");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installing_a_second_copy_of_a_plugin_is_reported() {
        let mut info = PluginInfo::with_name("计算器");
        info.repository = Some("https://github.com/hemingtsai/touchery".to_string());
        let manager = PluginManager {
            plugins: vec![
                plugin_with_info("calc.lua", info.clone()),
                // Same declared name, different file: a duplicate.
                plugin_with_info("calc-2.lua", PluginInfo::with_name("计算器")),
                // Same repository, different name: a duplicate too.
                plugin_with_info("other.lua", info.clone()),
                // Same file name is never reported against itself.
                plugin_with_info("third.lua", PluginInfo::with_name("別的")),
            ],
        };

        let duplicates = manager.duplicates_of("calc.lua");
        assert!(
            duplicates.contains(&"calc-2.lua".to_string()),
            "{duplicates:?}"
        );
        assert!(
            duplicates.contains(&"other.lua".to_string()),
            "{duplicates:?}"
        );
        assert!(!duplicates.contains(&"third.lua".to_string()));
        assert!(
            !duplicates.contains(&"calc.lua".to_string()),
            "a plugin is not its own duplicate"
        );
        assert!(manager.duplicates_of("missing.lua").is_empty());
    }

    #[test]
    fn an_installed_plugin_is_loaded_without_touching_the_others() {
        let dir = std::env::temp_dir().join("touchery-install-load-test");
        let _ = std::fs::remove_dir_all(&dir);
        let plugins_dir = dir.join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();

        let mut manager = PluginManager {
            plugins: Vec::new(),
        };

        // One plugin that is already installed and running.
        let existing = plugins_dir.join("existing.lua");
        std::fs::write(
            &existing,
            "function get_items(q) return {} end\nfunction run(v, q) end\nfunction run_sub(v, s) end\n",
        )
        .unwrap();
        manager.load_installed_from(&plugins_dir, "existing.lua", true);
        assert!(manager.plugins[0].runtime_loaded());

        // A broken script lands as a row with an error instead of vanishing.
        let broken = plugins_dir.join("broken.lua");
        std::fs::write(&broken, "this is not lua").unwrap();
        manager.load_installed_from(&plugins_dir, "broken.lua", true);
        assert_eq!(manager.plugins.len(), 2);
        let row = manager
            .plugins
            .iter()
            .find(|plugin| plugin.file_name == "broken.lua")
            .expect("the failed plugin keeps its row");
        assert!(row.error.is_some(), "the failure must be visible");
        assert!(!row.runtime_loaded());
        // The working plugin was not reloaded.
        assert!(
            manager
                .plugins
                .iter()
                .find(|plugin| plugin.file_name == "existing.lua")
                .unwrap()
                .runtime_loaded()
        );

        // Reinstalling the same name replaces the row instead of duplicating it.
        std::fs::write(
            &broken,
            "function get_items(q) return {} end\nfunction run(v, q) end\nfunction run_sub(v, s) end\n",
        )
        .unwrap();
        manager.load_installed_from(&plugins_dir, "broken.lua", true);
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

    #[test]
    fn installing_refuses_what_is_not_a_lua_script() {
        let dir = std::env::temp_dir().join("touchery-install-refuse-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let plugins_dir = dir.join("plugins");

        let not_lua = dir.join("script.txt");
        std::fs::write(&not_lua, "function get_items(q) return {} end").unwrap();
        assert!(install_into(&plugins_dir, &not_lua).is_err());
        assert!(!plugins_dir.exists(), "nothing may be written on refusal");

        let empty = dir.join("empty.lua");
        std::fs::write(&empty, "   \n").unwrap();
        assert!(install_into(&plugins_dir, &empty).is_err());

        let binary = dir.join("binary.lua");
        std::fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
        assert!(install_into(&plugins_dir, &binary).is_err());

        let missing = dir.join("missing.lua");
        assert!(install_into(&plugins_dir, &missing).is_err());

        // A directory that merely ends in .lua is not a script either.
        let fake = dir.join("folder.lua");
        std::fs::create_dir_all(&fake).unwrap();
        assert!(install_into(&plugins_dir, &fake).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_plugins_are_not_executed_at_startup() {
        let dir = std::env::temp_dir().join("touchery-plugin-disabled-test");
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("marker.txt");
        let _ = std::fs::remove_file(&marker);
        let plugin_path = write_probe_plugin(&dir, &marker);

        let mut plugins = Vec::new();
        PluginManager::load_file(&mut plugins, &plugin_path, false);

        assert!(!marker.exists(), "a disabled plugin must not run its chunk");
        assert_eq!(plugins.len(), 1);
        assert!(!plugins[0].enabled);
        assert!(!plugins[0].runtime_loaded());

        // Enabling it loads the runtime and executes the chunk.
        PluginManager::load_file(&mut plugins, &plugin_path, true);
        assert!(marker.exists(), "an enabled plugin must be loaded");
        assert!(plugins[1].enabled);
        assert!(plugins[1].runtime_loaded());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failing_plugin_is_reported_without_a_runtime() {
        let dir = std::env::temp_dir().join("touchery-plugin-broken-test");
        std::fs::create_dir_all(&dir).unwrap();
        let plugin_path = dir.join("broken.lua");
        std::fs::write(&plugin_path, "error('TOP_LEVEL_BOOM')").unwrap();

        let mut plugins = Vec::new();
        PluginManager::load_file(&mut plugins, &plugin_path, true);

        assert_eq!(plugins.len(), 1);
        assert!(
            plugins[0]
                .error
                .as_deref()
                .unwrap_or("")
                .contains("TOP_LEVEL_BOOM")
        );
        assert!(!plugins[0].runtime_loaded());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_sub_dispatches_to_the_secondary_handler() {
        let dir = std::env::temp_dir().join("touchery-plugin-dispatch-test");
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("out.txt");
        let _ = std::fs::remove_file(&log);
        let plugin_path = dir.join("dispatch.lua");
        std::fs::write(
            &plugin_path,
            format!(
                r##"
                local LOG = "{log}"
                local function write(line)
                    local f = assert(io.open(LOG, "a"))
                    f:write(line .. "\n")
                    f:close()
                end
                function get_items(query) return {{}} end
                function run(value, query) write("run:" .. value .. ":" .. query) end
                function run_sub(value, sub_query) write("run_sub:" .. value .. ":" .. sub_query) end
                "##,
                log = log.display()
            ),
        )
        .unwrap();

        let mut plugin = Plugin::load(&plugin_path).unwrap();
        plugin.run("v", "q");
        plugin.run_sub("v", "s");

        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "run:v:q\nrun_sub:v:s\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
