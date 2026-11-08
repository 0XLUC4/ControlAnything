//! Application assets: Lucide icons used by ControlAnything + gpui-kit default icons.

use std::borrow::Cow;

use gpui_kit::assets::{Assets, icon_assets};
use gpui_kit::{AssetSource, Result, SharedString};

// `Assets` only embeds the component icons: add just the ones we display.
// Already in `Assets`: Info, TriangleAlert, CircleCheck, CircleX, CircleAlert, Palette, HardDrive,
// Settings2, Copy, LoaderCircle, Inbox, RefreshCw, ArrowUp, ArrowDown, ArrowLeft, Mic, Network,
// Moon, Bell.
icon_assets!(
    ExtraIcons,
    [
        LayoutDashboard,
        Fan,
        Settings,
        Check,
        Undo2,
        X,
        Plus,
        Minus,
        Trash,
        Pencil,
        Download,
        FolderOpen,
        Play,
        Cpu,
        Gpu,
        MemoryStick,
        Thermometer,
        CircuitBoard,
        RotateCcw,
        ChevronDown,
        ChevronRight,
        Gauge,
        Lightbulb,
        Microchip,
        Droplets,
        LightbulbOff,
        ShieldAlert,
        Timer,
        Layers,
        SlidersHorizontal,
        ChartLine,
        ThermometerSun,
        Activity,
        HeartPulse,
        Rainbow,
        WavesHorizontal,
        SunDim,
        Unplug,
        Keyboard,
        Mouse,
        Volume2,
        VolumeX,
        Speaker,
        Headphones,
        Wifi,
        CircleArrowUp,
        Zap,
        Clock,
        FileDown,
    ]
);

/// Application logo (propeller), served to `img` elements.
pub const BRAND_ICON: &str = "brand/fan.png";

pub struct HubAssets;

impl AssetSource for HubAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == BRAND_ICON {
            return Ok(Some(Cow::Borrowed(crate::tray::FAN_PNG)));
        }
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        if BRAND_ICON.starts_with(path) {
            paths.push(BRAND_ICON.into());
        }
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::assets::IconName;

    #[test]
    fn used_icons_are_embedded() {
        for icon in [
            IconName::Fan,
            IconName::LayoutDashboard,
            IconName::Undo2,
            IconName::Microchip,
            IconName::Droplets,
            IconName::LightbulbOff,
            IconName::ShieldAlert,
            IconName::Timer,
            IconName::Layers,
            IconName::SlidersHorizontal,
            IconName::ChartLine,
            IconName::ThermometerSun,
            IconName::Activity,
            IconName::HeartPulse,
            IconName::Rainbow,
            IconName::WavesHorizontal,
            IconName::SunDim,
            IconName::Unplug,
            IconName::Keyboard,
            IconName::Mouse,
            IconName::Volume2,
            IconName::VolumeX,
            IconName::Speaker,
            IconName::Headphones,
            IconName::Wifi,
            IconName::CircleArrowUp,
            IconName::Zap,
            IconName::Clock,
            IconName::FileDown,
            IconName::Info,
            IconName::TriangleAlert,
            IconName::Network,
            IconName::Mic,
            IconName::Moon,
            IconName::Bell,
            IconName::ArrowLeft,
            IconName::Loader,
        ] {
            assert!(matches!(HubAssets.load(&icon.path()), Ok(Some(_))));
        }
        assert!(matches!(HubAssets.load(BRAND_ICON), Ok(Some(_))));
    }
}
