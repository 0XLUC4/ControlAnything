//! Global hotkeys Ctrl+Shift+F1..F4: activate profile no. 1..4 (order of `config.profiles`).
//!
//! Wayland does not allow global hotkeys: global-hotkey only handles X11 on Linux,
//! a registration failure is simply logged.
//! No Ctrl+Alt: on Windows AltGr = Ctrl+Alt, it would steal ~ # { on AZERTY.

use async_channel::Sender;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use hub_core::config::Config;

use crate::tray::Command;

const MODIFIERS: Modifiers = Modifiers::CONTROL.union(Modifiers::SHIFT);
const KEYS: [Code; 4] = [Code::F1, Code::F2, Code::F3, Code::F4];

fn profile_hotkeys() -> [HotKey; 4] {
    KEYS.map(|key| HotKey::new(Some(MODIFIERS), key))
}

/// Label shown next to profile no. `slot` (0..4), if it has a hotkey.
pub fn hotkey_label(slot: usize) -> Option<String> {
    (slot < KEYS.len()).then(|| format!("Ctrl+Shift+F{}", slot + 1))
}

/// Pressed hotkey → profile number.
fn slot_of(hotkey_id: u32) -> Option<usize> {
    profile_hotkeys().iter().position(|h| h.id() == hotkey_id)
}

/// Name of the profile activated by hotkey no. `slot`.
pub fn profile_for_slot(config: &Config, slot: usize) -> Option<&str> {
    config.profiles.get(slot).map(|p| p.name.as_str())
}

pub struct Hotkeys {
    manager: GlobalHotKeyManager,
    registered: bool,
}

impl Hotkeys {
    /// Must be called on the UI thread (Windows: the manager hidden window receives WM_HOTKEY there).
    pub fn new(commands: Sender<Command>) -> Result<Self, String> {
        let manager = GlobalHotKeyManager::new().map_err(|err| err.to_string())?;
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.state == HotKeyState::Pressed
                && let Some(slot) = slot_of(event.id)
            {
                let _ = commands.try_send(Command::ProfileSlot(slot));
            }
        }));
        Ok(Self {
            manager,
            registered: false,
        })
    }

    /// Registers or releases the hotkeys; a hotkey already taken elsewhere is ignored.
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled == self.registered {
            return;
        }
        for hotkey in profile_hotkeys() {
            let result = if enabled {
                self.manager.register(hotkey)
            } else {
                self.manager.unregister(hotkey)
            };
            if let Err(err) = result {
                tracing::warn!("global hotkey {hotkey}: {err}");
            }
        }
        self.registered = enabled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::config::Profile;

    #[test]
    fn hotkeys_map_to_profiles_in_order() {
        let ids: Vec<u32> = profile_hotkeys().iter().map(HotKey::id).collect();
        for (slot, id) in ids.iter().enumerate() {
            assert_eq!(slot_of(*id), Some(slot));
        }
        assert_eq!(slot_of(0), None);
        assert_eq!(hotkey_label(0).as_deref(), Some("Ctrl+Shift+F1"));
        assert_eq!(hotkey_label(4), None);

        let profile = |name: &str| Profile {
            name: name.into(),
            fans: Default::default(),
        };
        let config = Config {
            profiles: vec![profile("Silent"), profile("Gaming")],
            ..Config::default()
        };
        assert_eq!(profile_for_slot(&config, 1), Some("Gaming"));
        assert_eq!(profile_for_slot(&config, 3), None);
    }
}
