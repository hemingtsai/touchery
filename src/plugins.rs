use crate::config::Config;
use anyhow::Context as _;
use mlua::{Function, Lua, Table};
use std::path::PathBuf;

/// A single item produced by a plugin's `get_items`.
#[derive(Debug, Clone)]
pub struct PluginItem {
    pub title: String,
    pub value: String,
    /// If true, Enter opens the secondary-input flow (`run_sub`).
    pub sub: bool,
}

pub struct Plugin {
    pub file_name: String,
    pub name: String,
    pub enabled: bool,
    pub error: Option<String>,
    lua: Option<Lua>,
    get_items_fn: Option<Function>,
    run_fn: Option<Function>,
    run_sub_fn: Option<Function>,
}

/// Instruction budget per Lua call; exceeding it aborts the plugin so a
/// runaway script can never freeze the UI.
const INSTRUCTION_BUDGET: u32 = 20_000_000;

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
        lua.load(&source).exec()?;

        let get_items_fn = lua.globals().get::<Option<Function>>("get_items")?;
        let run_fn = lua.globals().get::<Option<Function>>("run")?;
        let run_sub_fn = lua.globals().get::<Option<Function>>("run_sub")?;
        if get_items_fn.is_none() || run_fn.is_none() {
            anyhow::bail!("plugin must define global functions `get_items(query)` and `run(value, query)`");
        }
        if run_sub_fn.is_none() {
            anyhow::bail!("plugin must define global function `run_sub(value, sub_query)`");
        }

        Ok(Self {
            file_name,
            name,
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
        Self {
            file_name,
            name,
            enabled,
            error: None,
            lua: None,
            get_items_fn: None,
            run_fn: None,
            run_sub_fn: None,
        }
    }

    /// Call with an execution budget; converts budget overrun into a normal error.
    ///
    /// Concurrency invariant: the set_hook/remove_hook pair is not atomic, so
    /// every caller must hold the `PluginManager` mutex while invoking this —
    /// which serializes all Lua access and makes hook mutation safe. (The
    /// launcher's background query task and run/run_sub threads all lock the
    /// manager for the duration of the call.)
    fn with_budget<T>(lua: &Lua, f: impl FnOnce() -> mlua::Result<T>) -> anyhow::Result<T> {
        lua.set_hook(
            mlua::HookTriggers {
                every_nth_instruction: Some(INSTRUCTION_BUDGET),
                ..mlua::HookTriggers::new()
            },
            |_, _| Err(mlua::Error::RuntimeError("execution budget exceeded".into())),
        )?;
        // Guard ensures remove_hook is called on all exit paths (including panics).
        struct HookGuard<'a>(&'a Lua);
        impl Drop for HookGuard<'_> {
            fn drop(&mut self) {
                self.0.remove_hook();
            }
        }
        let _guard = HookGuard(lua);
        let result = f();
        Ok(result?)
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
        match Self::with_budget(lua, || {
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
        let result = Self::with_budget(lua, || {
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
        assert!(plugins[0].error.as_deref().unwrap_or("").contains("TOP_LEVEL_BOOM"));
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
