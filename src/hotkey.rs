use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::GlobalHotKeyManager;

pub struct HotkeyState {
    pub id: u32,
    #[allow(dead_code)]
    pub manager: GlobalHotKeyManager,
}

impl HotkeyState {
    pub fn register() -> anyhow::Result<Self> {
        let hotkey = HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::Space);
        let manager = GlobalHotKeyManager::new()?;
        manager.register(hotkey)?;
        Ok(Self {
            id: hotkey.id(),
            manager,
        })
    }
}
