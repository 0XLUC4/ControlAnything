//! "Devices" page: all detected hardware. A lighting device opens its detail page on click.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Div, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use hub_core::Device;

use crate::app::{HubApp, Page};
use crate::lighting::device_icon;
use crate::state::{channel_rpm, display_name, is_pump, mode_label, settings_for};
use crate::widgets::{format_rpm, loading, muted, page_header, section, selectable_row};

/// Sensor groups, in display order.
const SENSOR_GROUPS: [(Device, &str, IconName); 5] = [
    (Device::Cpu, "CPU", IconName::Cpu),
    (Device::Gpu, "GPU", IconName::Gpu),
    (Device::Motherboard, "Motherboard", IconName::CircuitBoard),
    (Device::Memory, "Memory", IconName::MemoryStick),
    (Device::Storage, "Storage", IconName::HardDrive),
];

/// Leading icon, name and muted details of a device row.
fn device_info(icon: IconName, name: impl Into<SharedString>, details: String, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .flex_1()
        .min_w_0()
        .gap_3()
        .items_center()
        .child(Icon::new(icon).text_color(theme.muted_foreground))
        .child(
            v_flex()
                .min_w_0()
                .child(div().text_sm().font_semibold().child(name.into()))
                .child(muted(details, cx).text_xs()),
        )
}

/// "Configure" affordance at the end of a clickable row.
fn configure_hint(id: impl Into<gpui_kit::ElementId>) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .label("Configure")
        .icon(IconName::ChevronRight)
}

impl HubApp {
    pub fn render_devices(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let header = page_header(
            IconName::Microchip,
            "Devices",
            "Hardware detected by the service",
            cx,
        );
        let Some(draft) = self.state.draft.as_ref() else {
            return loading("Waiting for the service configuration...", cx);
        };

        let mut lighting = section(IconName::Lightbulb, "Lighting devices", cx);
        if self.state.rgb_devices.is_empty() {
            lighting = lighting.child(muted("No RGB device detected", cx));
        }
        for (i, device) in self.state.rgb_devices.iter().enumerate() {
            let leds: usize = device.zones.iter().map(|z| z.leds).sum();
            let controlled = !draft.rgb.disabled.contains(&device.id);
            let id = device.id.clone();
            let switch = Switch::new(("rgb-controlled", i))
                .checked(controlled)
                .tooltip("Controlled by ControlAnything")
                .on_click(cx.listener(move |app, checked: &bool, _, cx| {
                    let checked = *checked;
                    app.edit_config(cx, |c| {
                        if checked {
                            c.rgb.disabled.remove(&id);
                        } else {
                            c.rgb.disabled.insert(id.clone());
                        }
                    });
                }));
            let details = format!("{} zones, {leds} LEDs", device.zones.len());
            let open = device.id.clone();
            let row = selectable_row(("rgb-device", i), false, cx)
                .flex_1()
                .flex_row()
                .items_center()
                .child(device_info(
                    device_icon(device),
                    device.label.clone(),
                    details,
                    cx,
                ))
                .when(!controlled, |row| {
                    row.child(Tag::secondary().outline().small().child("Released"))
                })
                .child(configure_hint(("rgb-configure", i)))
                .on_click(cx.listener(move |app, _, _, cx| app.open_device(open.clone(), cx)));
            // The switch stays outside the clickable row: toggling it does not open the page.
            lighting = lighting.child(h_flex().gap_2().items_center().child(row).child(switch));
        }

        let mut fans = section(IconName::Fan, "Fan channels", cx);
        if self.state.channels.is_empty() {
            fans = fans.child(muted("No fan channel detected", cx));
        }
        for (i, channel) in self.state.channels.iter().enumerate() {
            let rpm = format_rpm(channel_rpm(channel, self.state.readings()));
            let mode = mode_label(&settings_for(draft, &channel.id).mode);
            let icon = if is_pump(channel) {
                IconName::Droplets
            } else {
                IconName::Fan
            };
            let id = channel.id.clone();
            let row = selectable_row(("fan-device", i), false, cx)
                .flex_row()
                .items_center()
                .child(device_info(
                    icon,
                    display_name(Some(draft), channel),
                    channel.id.clone(),
                    cx,
                ))
                .child(Tag::secondary().outline().small().child(rpm))
                .child(Tag::info().small().child(mode))
                .child(configure_hint(("fan-configure", i)))
                .on_click(cx.listener(move |app, _, _, cx| {
                    app.fans.selected = Some(id.clone());
                    app.fans.curve.dragging = None;
                    app.page = Page::Fans;
                    cx.notify();
                }));
            fans = fans.child(row);
        }

        let readings = self.state.readings();
        let mut sensors = section(IconName::Thermometer, "Sensors", cx);
        if readings.is_empty() {
            sensors = sensors.child(muted("No sensor reading yet", cx));
        }
        for (device, label, icon) in SENSOR_GROUPS {
            let count = readings.iter().filter(|r| r.device == device).count();
            if count > 0 {
                sensors = sensors.child(
                    h_flex()
                        .justify_between()
                        .px_3()
                        .child(
                            h_flex()
                                .gap_3()
                                .items_center()
                                .text_sm()
                                .child(Icon::new(icon).text_color(cx.theme().muted_foreground))
                                .child(label),
                        )
                        .child(muted(format!("{count} sensors"), cx)),
                );
            }
        }

        v_flex()
            .gap_4()
            .max_w(px(760.))
            .child(header)
            .child(lighting)
            .child(fans)
            .child(sensors)
            .into_any_element()
    }
}
