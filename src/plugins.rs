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
        self.run_impl(|run_fn| run_fn.call::<()>((value.to_string(), query.to_string())))
    }

    pub fn run_sub(&mut self, value: &str, sub_query: &str) {
        self.run_impl(|run_fn| run_fn.call::<()>((value.to_string(), sub_query.to_string())))
    }

    fn run_impl(&mut self, f: impl FnOnce(Function) -> mlua::Result<()>) {
        let Some(lua) = self.lua.as_ref() else {
            return;
        };
        let Some(run_fn) = self.run_fn.clone() else {
            return;
        };
        if let Err(e) = Self::with_budget(lua, || f(run_fn)) {
            eprintln!("[plugin:{}] run error: {e}", self.name);
            self.error = Some(format!("run 失败: {e}"));
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

            match Plugin::load(&path) {
                Ok(mut plugin) => {
                    plugin.enabled = enabled;
                    self.plugins.push(plugin);
                }
                Err(e) => {
                    eprintln!("[plugin] failed to load {}: {e:#}", path.display());
                    self.plugins.push(Plugin {
                        file_name,
                        name: path
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        enabled,
                        error: Some(format!("{e:#}")),
                        lua: None,
                        get_items_fn: None,
                        run_fn: None,
                        run_sub_fn: None,
                    });
                }
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
