//! "Audio" page: output and input devices, volume, mute, default device.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Div, Entity, IntoElement, ParentElement as _,
    Styled as _, Subscription, div, px,
};

use crate::app::HubApp;
use crate::system::audio::{self, Endpoint};
use crate::widgets::{empty_state, muted, page_header, section};

/// Endpoints are re-read at most this often (the app ticks every second).
const REFRESH: Duration = Duration::from_millis(900);
/// After a drag, external volume changes wait this long before moving the slider.
const DRAG_GRACE: Duration = Duration::from_millis(1500);
/// Volume writes while dragging are coalesced to at most one batch per interval.
const VOLUME_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Default)]
pub struct AudioPage {
    /// `None` until the page is first shown.
    endpoints: Option<Result<Vec<Endpoint>, String>>,
    refreshed: Option<Instant>,
    /// Volume slider per endpoint id (percent).
    sliders: HashMap<String, (Entity<SliderState>, Subscription)>,
    dragged: Option<Instant>,
    /// A `list()` runs on a worker thread.
    loading: bool,
    /// Latest unwritten volume (0.0 to 1.0) per endpoint id.
    pending_volumes: HashMap<String, f32>,
    /// The volume writer task is running.
    writing: bool,
}

impl AudioPage {
    /// Records the latest volume; true when no writer runs yet and one must start.
    fn queue_volume(&mut self, id: String, volume: f32) -> bool {
        self.pending_volumes.insert(id, volume);
        !std::mem::replace(&mut self.writing, true)
    }

    /// Next batch to write, or `None` (and the writer stops) when nothing is left.
    fn take_volumes(&mut self) -> Option<Vec<(String, f32)>> {
        if self.pending_volumes.is_empty() {
            self.writing = false;
            return None;
        }
        Some(self.pending_volumes.drain().collect())
    }
}

fn warn_on_error(action: &str, result: Result<(), String>) {
    if let Err(err) = result {
        tracing::warn!("audio: {action} failed: {err}");
    }
}

fn volume_slider(
    id: String,
    percent: f32,
    cx: &mut Context<HubApp>,
) -> (Entity<SliderState>, Subscription) {
    let slider = cx.new(|_| {
        SliderState::new()
            .min(0.)
            .max(100.)
            .step(1.)
            .default_value(percent)
    });
    let subscription = cx.subscribe(&slider, move |app, _, event: &SliderEvent, cx| {
        if let SliderEvent::Change(value) = event {
            app.system.audio.dragged = Some(Instant::now());
            app.write_volume(id.clone(), value.start() / 100., cx);
            cx.notify();
        }
    });
    (slider, subscription)
}

impl HubApp {
    /// Writes volumes off the UI thread, latest value wins (COM can block on a slow driver).
    fn write_volume(&mut self, id: String, volume: f32, cx: &mut Context<Self>) {
        if !self.system.audio.queue_volume(id, volume) {
            return;
        }
        cx.spawn(async move |this, cx| {
            while let Ok(Some(batch)) = this.update(cx, |app, _| app.system.audio.take_volumes()) {
                cx.background_executor()
                    .spawn(async move {
                        for (id, volume) in batch {
                            warn_on_error("volume", audio::set_volume(&id, volume));
                        }
                    })
                    .await;
                cx.background_executor().timer(VOLUME_INTERVAL).await;
            }
        })
        .detach();
    }

    /// Runs a mute or default change off the UI thread, then re-reads the endpoints.
    fn change_audio(
        &mut self,
        action: &'static str,
        work: impl FnOnce() -> Result<(), String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { work() }).await;
            warn_on_error(action, result);
            let _ = this.update(cx, |app, cx| app.refresh_audio(true, cx));
        })
        .detach();
    }

    /// Starts a background `list()` unless one runs or the last one is recent.
    fn refresh_audio(&mut self, force: bool, cx: &mut Context<Self>) {
        let page = &mut self.system.audio;
        if page.loading {
            // Re-read as soon as the running list completes.
            if force {
                page.refreshed = None;
            }
            return;
        }
        if !force && page.refreshed.is_some_and(|at| at.elapsed() < REFRESH) {
            return;
        }
        page.refreshed = Some(Instant::now());
        page.loading = true;
        cx.spawn(async move |this, cx| {
            let endpoints = cx
                .background_executor()
                .spawn(async { audio::list() })
                .await;
            let _ = this.update(cx, |app, cx| {
                app.system.audio.loading = false;
                app.apply_audio(endpoints, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_audio(&mut self, endpoints: Result<Vec<Endpoint>, String>, cx: &mut Context<Self>) {
        let page = &mut self.system.audio;
        if let Ok(endpoints) = &endpoints {
            // Recreate a slider when the volume changed elsewhere (keys, Windows mixer),
            // unless the user is dragging it.
            let dragging = page.dragged.is_some_and(|at| at.elapsed() < DRAG_GRACE);
            page.sliders
                .retain(|id, _| endpoints.iter().any(|e| &e.id == id));
            for endpoint in endpoints {
                let percent = (endpoint.volume * 100.).round();
                let stale = page.sliders.get(&endpoint.id).is_none_or(|(slider, _)| {
                    !dragging && (slider.read(cx).value().start() - percent).abs() >= 1.
                });
                if stale {
                    let slider = volume_slider(endpoint.id.clone(), percent, cx);
                    page.sliders.insert(endpoint.id.clone(), slider);
                }
            }
        }
        page.endpoints = Some(endpoints);
    }

    pub fn render_audio(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.refresh_audio(false, cx);
        let header = page_header(IconName::Volume2, "Audio", "Output and input devices", cx);
        let endpoints = match &self.system.audio.endpoints {
            Some(Ok(endpoints)) => endpoints.clone(),
            Some(Err(err)) => {
                return v_flex()
                    .gap_4()
                    .child(header)
                    .child(empty_state(
                        IconName::VolumeX,
                        "Audio unavailable",
                        err.clone(),
                        cx,
                    ))
                    .into_any_element();
            }
            None => Vec::new(),
        };
        let mut output = section(IconName::Speaker, "Output", cx);
        let mut input = section(IconName::Mic, "Input", cx);
        for (i, endpoint) in endpoints.iter().enumerate() {
            let row = self.endpoint_row(i, endpoint, cx);
            if endpoint.input {
                input = input.child(row);
            } else {
                output = output.child(row);
            }
        }
        if !endpoints.iter().any(|e| !e.input) {
            output = output.child(muted("No output device", cx));
        }
        if !endpoints.iter().any(|e| e.input) {
            input = input.child(muted("No input device", cx));
        }
        v_flex()
            .gap_4()
            .child(header)
            .child(output)
            .child(input)
            .into_any_element()
    }

    /// Icon, name, mute, volume slider and default device control.
    fn endpoint_row(&self, i: usize, endpoint: &Endpoint, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let icon = match (endpoint.input, endpoint.headphones) {
            (true, _) => IconName::Mic,
            (false, true) => IconName::Headphones,
            (false, false) => IconName::Speaker,
        };
        let slider = self.system.audio.sliders.get(&endpoint.id).map(|(s, _)| s);
        let percent = slider.map_or(endpoint.volume * 100., |s| s.read(cx).value().start());

        let id = endpoint.id.clone();
        let muted_now = endpoint.muted;
        let mute = Button::new(("audio-mute", i))
            .ghost()
            .small()
            .icon(if muted_now {
                IconName::VolumeX
            } else {
                IconName::Volume2
            })
            .tooltip(if muted_now { "Unmute" } else { "Mute" })
            .on_click(cx.listener(move |app, _, _, cx| {
                let id = id.clone();
                app.change_audio("mute", move || audio::set_mute(&id, !muted_now), cx);
            }));

        let default: AnyElement = if endpoint.is_default {
            Tag::info().small().child("Default").into_any_element()
        } else {
            let id = endpoint.id.clone();
            Button::new(("audio-default", i))
                .outline()
                .small()
                .label("Set as default")
                .on_click(cx.listener(move |app, _, _, cx| {
                    let id = id.clone();
                    app.change_audio("set default", move || audio::set_default(&id), cx);
                }))
                .into_any_element()
        };

        h_flex()
            .gap_3()
            .items_center()
            .child(Icon::new(icon).small().text_color(theme.muted_foreground))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_semibold()
                    .child(endpoint.name.clone()),
            )
            .child(mute)
            .children(slider.map(|s| div().w(px(200.)).child(Slider::new(s))))
            .child(
                div()
                    .w(px(44.))
                    .text_right()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(format!("{percent:.0} %")),
            )
            .child(div().w(px(120.)).flex().justify_end().child(default))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_writes_coalesce_latest_wins() {
        let mut page = AudioPage::default();
        assert!(
            page.queue_volume("a".into(), 0.1),
            "first value starts the writer"
        );
        assert!(
            !page.queue_volume("a".into(), 0.4),
            "writer already running"
        );
        assert!(!page.queue_volume("b".into(), 0.7));
        let mut batch = page.take_volumes().unwrap();
        batch.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(batch, vec![("a".into(), 0.4), ("b".into(), 0.7)]);
        assert_eq!(page.take_volumes(), None);
        assert!(
            page.queue_volume("a".into(), 0.5),
            "stopped writer restarts"
        );
    }
}
