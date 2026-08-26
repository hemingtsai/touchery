use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::GlobalHotKeyManager;

pub fn default_hotkey() -> HotKey {
    HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::Space)
}

pub struct HotkeyState {
    pub id: u32,
    #[allow(dead_code)]
    pub manager: GlobalHotKeyManager,
}

impl HotkeyState {
    pub fn register(hotkey: HotKey) -> anyhow::Result<Self> {
        let manager = GlobalHotKeyManager::new()?;
        manager.register(hotkey)?;
        Ok(Self {
            id: hotkey.id(),
            manager,
        })
    }
}
