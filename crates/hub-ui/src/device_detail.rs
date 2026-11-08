//! Detail page of one RGB device (Devices page): control switch, zones with a live
//! preview, per-device brightness, effect and color picker (whole device or one zone).
//! Edits go to the draft `RgbConfig` like the Lighting page, the service applies them.

use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, Window, px,
};

use crate::app::HubApp;
use crate::lighting::{
    colors, current_effect, device_icon, percent_slider, sync_controls, sync_slider, zone_effect,
    zone_key, zone_row,
};
use crate::widgets::{empty_state, field, loading, muted, page_header, section};

impl HubApp {
    pub fn render_device_detail(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let back = Button::new("device-back")
            .ghost()
            .small()
            .icon(IconName::ArrowLeft)
            .label("All devices")
            .on_click(cx.listener(|app, _, _, cx| {
                app.device_detail = None;
                cx.notify();
            }));
        let (Some(draft), Some(controls)) = (
            self.state.draft.as_ref(),
            self.controls.as_ref().map(|c| c.lighting.clone()),
        ) else {
            return loading("Waiting for the service configuration...", cx);
        };
        let Some(device) = self.state.rgb_devices.iter().find(|d| d.id == id).cloned() else {
            return v_flex()
                .gap_4()
                .child(h_flex().child(back))
                .child(empty_state(
                    IconName::Unplug,
                    "Device not found",
                    "The service no longer detects this device.",
                    cx,
                ))
                .into_any_element();
        };
        let rgb = draft.rgb.clone();
        let leds: usize = device.zones.iter().map(|z| z.leds).sum();
        let controlled = !rgb.disabled.contains(&device.id);
        let header = page_header(
            device_icon(&device),
            device.label.clone(),
            format!("{} zones, {leds} LEDs", device.zones.len()),
            cx,
        )
        .child(back);

        let device_id = device.id.clone();
        let switch = Switch::new("detail-controlled")
            .checked(controlled)
            .on_click(cx.listener(move |app, checked: &bool, _, cx| {
                let checked = *checked;
                app.edit_config(cx, |c| {
                    if checked {
                        c.rgb.disabled.remove(&device_id);
                    } else {
                        c.rgb.disabled.insert(device_id.clone());
                    }
                });
            }));
        let control = section(IconName::SlidersHorizontal, "Control", cx)
            .child(field("Controlled by ControlAnything", switch, cx))
            .child(muted(device.id.clone(), cx).text_xs());
        let page = v_flex()
            .gap_4()
            .max_w(px(760.))
            .child(header)
            .child(control);
        if !controlled {
            return page
                .child(Alert::info(
                    "detail-released",
                    "Released: firmware or vendor software controls this device. Turn control on to set its colors here.",
                ))
                .into_any_element();
        }

        let zones = self.edited_zones();
        let whole_device = zones.len() > 1 || device.zones.len() == 1;
        let effect = current_effect(&rgb, zones.first().map(String::as_str));
        let effect_colors = effect.as_ref().map(colors).unwrap_or_default();
        sync_controls(&controls, rgb.brightness, &effect_colors, window, cx);
        let device_level = rgb.device_brightness.get(id).copied().unwrap_or(100);
        sync_slider(&controls.device_brightness, device_level, window, cx);

        let mut page = page;
        if rgb.sync.is_some() {
            let per_device = Button::new("detail-unsync")
                .outline()
                .small()
                .icon(IconName::Layers)
                .label("Set effects per device")
                .on_click(cx.listener(|app, _, _, cx| app.edit_config(cx, |c| c.rgb.sync = None)));
            page = page
                .child(Alert::warning(
                    "detail-sync",
                    "\"Same effect on all devices\" is on: the effect below applies to every device.",
                ))
                .child(h_flex().child(per_device));
        }

        // Zone list: live preview of each zone, click to edit one zone or the whole device.
        let mut zones_card = section(IconName::Layers, "Zones", cx);
        if device.zones.len() > 1 && rgb.sync.is_none() {
            let first = zone_effect(&rgb, &zone_key(&device.id, &device.zones[0].id));
            zones_card = zones_card.child(
                zone_row(
                    "detail-whole",
                    "Whole device",
                    "Same effect on every zone",
                    &first,
                    whole_device,
                    cx,
                )
                .on_click(cx.listener(|app, _, _, cx| {
                    app.lighting.detail_zone = None;
                    cx.notify();
                })),
            );
        }
        for (i, zone) in device.zones.iter().enumerate() {
            let key = zone_key(&device.id, &zone.id);
            let active = zones == [key.clone()];
            let row = zone_row(
                ("detail-zone", i),
                zone.label.clone(),
                format!("{} LEDs", zone.leds),
                &zone_effect(&rgb, &key),
                active,
                cx,
            )
            .on_click(cx.listener(move |app, _, _, cx| {
                app.lighting.detail_zone = Some(key.clone());
                cx.notify();
            }));
            zones_card = zones_card.child(row);
        }

        let brightness = section(IconName::SunDim, "Brightness", cx)
            .child(field(
                "Device brightness",
                percent_slider(&controls.device_brightness, device_level),
                cx,
            ))
            .child(muted(
                format!(
                    "Scaled by the global brightness ({} %) of the Lighting page.",
                    rgb.brightness
                ),
                cx,
            ));

        let title = if rgb.sync.is_some() {
            "Effect: all devices".to_owned()
        } else if whole_device {
            "Effect: whole device".to_owned()
        } else {
            let label = device
                .zones
                .iter()
                .find(|z| zones.first() == Some(&zone_key(&device.id, &z.id)))
                .map_or("zone", |z| z.label.as_str());
            format!("Effect: {label}")
        };
        let mut page = page.child(zones_card).child(brightness);
        if let Some(effect) = effect {
            page = page.child(self.effect_editor(title, &effect, &controls, cx));
        }
        page.into_any_element()
    }
}
