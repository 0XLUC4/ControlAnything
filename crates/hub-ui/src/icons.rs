//! Assets de l'application : icônes Lucide utilisées par ControlAnything + icônes par défaut de gpui-kit.

use std::borrow::Cow;

use gpui_kit::assets::{Assets, icon_assets};
use gpui_kit::{AssetSource, Result, SharedString};

// `Assets` n'embarque que les icônes des composants : on ajoute uniquement celles qu'on affiche.
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
    ]
);

/// Logo de l'application (hélice), servi aux éléments `img`.
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
        for icon in [IconName::Fan, IconName::LayoutDashboard, IconName::Undo2] {
            assert!(matches!(HubAssets.load(&icon.path()), Ok(Some(_))));
        }
        assert!(matches!(HubAssets.load(BRAND_ICON), Ok(Some(_))));
    }
}
