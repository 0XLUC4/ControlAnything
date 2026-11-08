//! System tray icon: open, switch profile, quit.
//!
//! Windows: the icon lives on the UI thread (GPUI pumps the Win32 messages of the whole thread).
//! Linux: tray-icon requires GTK, which runs on a dedicated thread; profiles are sent to it over a channel.

use async_channel::Sender;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const OPEN: &str = "open";
const QUIT: &str = "quit";
const PROFILE_PREFIX: &str = "profile:";
pub const FAN_PNG: &[u8] = include_bytes!("../../../assets/icons/fan.png");

/// Action requested from the system tray or a global hotkey.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Open,
    Quit,
    SetProfile(String),
    /// Ctrl+Shift+F1..F4: profile no. `slot` (order of `config.profiles`).
    ProfileSlot(usize),
}

/// Menu item id → command.
fn menu_command(id: &str) -> Option<Command> {
    match id {
        OPEN => Some(Command::Open),
        QUIT => Some(Command::Quit),
        _ => id
            .strip_prefix(PROFILE_PREFIX)
            .map(|name| Command::SetProfile(name.to_owned())),
    }
}

/// RGBA pixels of the fan icon.
fn fan_rgba() -> Result<(Vec<u8>, u32, u32), String> {
    let image = image::load_from_memory_with_format(FAN_PNG, image::ImageFormat::Png)
        .map_err(|err| err.to_string())?
        .into_rgba8();
    let (width, height) = image.dimensions();
    Ok((image.into_raw(), width, height))
}

/// The handlers (called outside GPUI) forward events to the UI.
fn forward_events(commands: Sender<Command>) {
    let menu_commands = commands.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if let Some(command) = menu_command(event.id.as_ref()) {
            let _ = menu_commands.try_send(command);
        }
    }));
    // Left click on the icon: open (not emitted by AppIndicator on Linux: "Open" menu).
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let _ = commands.try_send(Command::Open);
        }
    }));
}

/// Icon and menu; must be used on the thread that pumps its messages.
struct TrayMenu {
    _icon: TrayIcon,
    profiles: Submenu,
    names: Vec<String>,
}

impl TrayMenu {
    fn build() -> Result<Self, String> {
        let (rgba, width, height) = fan_rgba()?;
        let icon =
            tray_icon::Icon::from_rgba(rgba, width, height).map_err(|err| err.to_string())?;
        let profiles = Submenu::new("Profile", true);
        let menu = Menu::new();
        menu.append_items(&[
            &MenuItem::with_id(OPEN, "Open ControlAnything", true, None),
            &profiles,
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(
                QUIT,
                "Quit (the daemon keeps controlling the fans)",
                true,
                None,
            ),
        ])
        .map_err(|err| err.to_string())?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("ControlAnything")
            .with_icon(icon)
            .build()
            .map_err(|err| err.to_string())?;
        Ok(Self {
            _icon: tray,
            profiles,
            names: Vec::new(),
        })
    }

    /// Rebuilds the list if it changed, then re-checks the active profile
    /// (a click checks the item even if the daemon rejects the change).
    fn set_profiles(&mut self, names: &[String], active: &str) {
        if names != self.names {
            while self.profiles.remove_at(0).is_some() {}
            for name in names {
                let item = CheckMenuItem::with_id(
                    format!("{PROFILE_PREFIX}{name}"),
                    name.replace('&', "&&"),
                    true,
                    false,
                    None,
                );
                if let Err(err) = self.profiles.append(&item) {
                    tracing::warn!("system tray menu: {err}");
                }
            }
            self.names = names.to_vec();
        }
        let active_id = format!("{PROFILE_PREFIX}{active}");
        for item in self.profiles.items() {
            if let Some(check) = item.as_check_menuitem() {
                check.set_checked(check.id().as_ref() == active_id);
            }
        }
    }
}

pub struct Tray {
    #[cfg(not(target_os = "linux"))]
    menu: TrayMenu,
    #[cfg(target_os = "linux")]
    updates: std::sync::mpsc::Sender<(Vec<String>, String)>,
}

impl Tray {
    /// Must be called on the GPUI UI thread.
    #[cfg(not(target_os = "linux"))]
    pub fn new(commands: Sender<Command>) -> Result<Self, String> {
        let menu = TrayMenu::build()?;
        forward_events(commands);
        Ok(Self { menu })
    }

    #[cfg(target_os = "linux")]
    pub fn new(commands: Sender<Command>) -> Result<Self, String> {
        use std::sync::mpsc;
        use std::time::Duration;

        let (updates, received) = mpsc::channel::<(Vec<String>, String)>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        std::thread::spawn(move || {
            let built = gtk::init()
                .map_err(|err| err.to_string())
                .and_then(|()| TrayMenu::build());
            let mut menu = match built {
                Ok(menu) => menu,
                Err(err) => {
                    let _ = ready_tx.send(Err(err));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            gtk::glib::timeout_add_local(Duration::from_millis(200), move || {
                while let Ok((names, active)) = received.try_recv() {
                    menu.set_profiles(&names, &active);
                }
                gtk::glib::ControlFlow::Continue
            });
            gtk::main();
        });
        ready_rx.recv().map_err(|err| err.to_string())??;
        forward_events(commands);
        Ok(Self { updates })
    }

    pub fn set_profiles(&mut self, names: Vec<String>, active: String) {
        #[cfg(not(target_os = "linux"))]
        self.menu.set_profiles(&names, &active);
        #[cfg(target_os = "linux")]
        let _ = self.updates.send((names, active));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_map_to_commands() {
        assert_eq!(menu_command(OPEN), Some(Command::Open));
        assert_eq!(menu_command(QUIT), Some(Command::Quit));
        assert_eq!(
            menu_command("profile:Games & Co"),
            Some(Command::SetProfile("Games & Co".into()))
        );
        assert_eq!(menu_command("other"), None);
    }

    #[test]
    fn fan_icon_decodes() {
        let (rgba, width, height) = fan_rgba().expect("valid PNG icon");
        assert_eq!((width, height), (64, 64));
        assert_eq!(rgba.len(), 64 * 64 * 4);
    }
}
