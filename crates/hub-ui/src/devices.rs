//! "Devices" page: all detected hardware, and which lighting devices the app controls.

use gpui_kit::component::button::Button;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use hub_core::Device;
use hub_core::config::FanMode;

use crate::app::{HubApp, Page};
use crate::fans::waiting;
use crate::state::{channel_rpm, display_name, settings_for};
use crate::widgets::{field, muted, section};

/// Sensor groups, in display order.
const SENSOR_GROUPS: [(Device, &str); 5] = [
    (Device::Cpu, "CPU"),
    (Device::Gpu, "GPU"),
    (Device::Motherboard, "Motherboard"),
    (Device::Memory, "Memory"),
    (Device::Storage, "Storage"),
];

/// Device row: name with a discreet id and details below, control on the right.
fn device_row(
    name: impl Into<SharedString>,
    details: String,
    control: impl IntoElement,
    cx: &Context<HubApp>,
) -> Div {
    let theme = cx.theme();
    h_flex()
        .justify_between()
        .gap_4()
        .child(
            v_flex()
                .min_w_0()
                .child(div().text_sm().font_semibold().child(name.into()))
                .child(div().text_xs().text_color(theme.muted_foreground).child(details)),
        )
        .child(control)
}

fn mode_label(mode: &FanMode) -> String {
    match mode {
        FanMode::Auto => "Auto".into(),
        FanMode::Fixed { duty } => format!("Fixed {duty} %"),
        FanMode::Curve { .. } => "Curve".into(),
    }
}

impl HubApp {
    pub fn render_devices(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(draft) = self.state.draft.as_ref() else {
            return waiting(cx);
        };

        let mut lighting = section("Lighting devices", cx);
        if self.state.rgb_devices.is_empty() {
            lighting = lighting.child(muted("No RGB device detected", cx));
        }
        for (i, device) in self.state.rgb_devices.iter().enumerate() {
            let leds: usize = device.zones.iter().map(|z| z.leds).sum();
            let controlled = !draft.rgb.disabled.contains(&device.id);
            let id = device.id.clone();
            let switch = Switch::new(("rgb-controlled", i))
                .checked(controlled)
                .label("Controlled by ControlAnything")
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
            let details = format!("{}, {} zones, {leds} LEDs", device.id, device.zones.len());
            lighting = lighting.child(
                v_flex()
                    .gap_1()
                    .child(device_row(device.label.clone(), details, switch, cx))
                    .when(!controlled, |row| {
                        row.child(muted("Released: firmware or vendor software can control it.", cx))
                    }),
            );
        }

        let mut fans = section("Fan channels", cx);
        if self.state.channels.is_empty() {
            fans = fans.child(muted("No fan channel detected", cx));
        }
        for (i, channel) in self.state.channels.iter().enumerate() {
            let rpm = channel_rpm(channel, self.state.readings())
                .map_or("- RPM".into(), |rpm| format!("{rpm:.0} RPM"));
            let mode = mode_label(&settings_for(draft, &channel.id).mode);
            let id = channel.id.clone();
            let edit = Button::new(("edit-fan", i))
                .outline()
                .small()
                .label("Edit")
                .on_click(cx.listener(move |app, _, _, cx| {
                    app.fans.selected = Some(id.clone());
                    app.fans.curve.dragging = None;
                    app.page = Page::Fans;
                    cx.notify();
                }));
            fans = fans.child(device_row(
                display_name(Some(draft), channel),
                format!("{}, {rpm}, {mode}", channel.id),
                edit,
                cx,
            ));
        }

        let readings = self.state.readings();
        let mut sensors = section("Sensors", cx);
        if readings.is_empty() {
            sensors = sensors.child(muted("No sensor reading yet", cx));
        }
        for (device, label) in SENSOR_GROUPS {
            let count = readings.iter().filter(|r| r.device == device).count();
            if count > 0 {
                sensors = sensors.child(field(label, div().text_sm().child(format!("{count} sensors")), cx));
            }
        }

        v_flex()
            .gap_4()
            .max_w(px(760.))
            .child(lighting)
            .child(fans)
            .child(sensors)
            .into_any_element()
    }
}
