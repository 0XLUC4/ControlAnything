//! "Network" page: adapters, addresses and live traffic.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::chart::AreaChart;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Div, IntoElement, ParentElement as _, SharedString, Styled as _, div,
    px,
};

use crate::app::HubApp;
use crate::system::net::{self, Adapter, format_bytes, format_link_speed, format_rate, rate};
use crate::widgets::{
    GREEN, SERIES_CPU, SERIES_GPU, empty_state, hex, muted, page_header, section, solid_tag,
    stat_line,
};

/// Adapters are re-read at most this often (the app ticks every second).
const REFRESH: Duration = Duration::from_millis(900);
/// Beyond this gap (page hidden), a rate would be a misleading average: skip it.
const MAX_GAP_SECS: f64 = 3.0;
/// Points of the traffic chart (one per refresh).
const HISTORY: usize = 60;
const DOWN_COLOR: u32 = SERIES_CPU;
const UP_COLOR: u32 = SERIES_GPU;

/// Received / sent pair (bytes, or bytes per second).
type Pair = (f64, f64);

#[derive(Default)]
pub struct NetworkPage {
    /// `None` until the page is first shown.
    adapters: Option<Result<Vec<Adapter>, String>>,
    sampled: Option<Instant>,
    /// A `list()` runs on a worker thread.
    loading: bool,
    /// Counters per adapter LUID: previous sample, and first sample of the session.
    previous: HashMap<u64, (u64, u64)>,
    baseline: HashMap<u64, (u64, u64)>,
    rates: HashMap<u64, Pair>,
    /// Total download / upload rate of all adapters, oldest first.
    history: VecDeque<Pair>,
}

impl NetworkPage {
    /// Stores a finished `list()`, sampled now.
    fn apply(&mut self, adapters: Result<Vec<Adapter>, String>) {
        self.loading = false;
        let now = Instant::now();
        let secs = self.sampled.map(|at| (now - at).as_secs_f64());
        self.sampled = Some(now);
        if let Ok(adapters) = &adapters {
            self.sample(adapters, secs);
        }
        self.adapters = Some(adapters);
    }

    /// Updates rates and history from new counters, `secs` after the previous sample.
    fn sample(&mut self, adapters: &[Adapter], secs: Option<f64>) {
        let secs = secs.filter(|secs| *secs <= MAX_GAP_SECS);
        self.rates.clear();
        for adapter in adapters {
            let counters = (adapter.rx_bytes, adapter.tx_bytes);
            self.baseline.entry(adapter.luid).or_insert(counters);
            if let (Some(secs), Some(&(rx, tx))) = (secs, self.previous.get(&adapter.luid)) {
                let pair = (rate(rx, counters.0, secs), rate(tx, counters.1, secs));
                self.rates.insert(adapter.luid, pair);
            }
            self.previous.insert(adapter.luid, counters);
        }
        if secs.is_some() {
            if self.history.len() == HISTORY {
                self.history.pop_front();
            }
            self.history.push_back(self.total_rate());
        }
    }

    fn total_rate(&self) -> Pair {
        self.rates
            .values()
            .fold((0., 0.), |sum, rate| (sum.0 + rate.0, sum.1 + rate.1))
    }

    /// Bytes received / sent since the adapter was first seen in this session.
    fn session_bytes(&self, adapter: &Adapter) -> Pair {
        let (rx, tx) = self
            .baseline
            .get(&adapter.luid)
            .copied()
            .unwrap_or((adapter.rx_bytes, adapter.tx_bytes));
        (
            adapter.rx_bytes.saturating_sub(rx) as f64,
            adapter.tx_bytes.saturating_sub(tx) as f64,
        )
    }
}

/// Arrow icon + value, colored like its chart series.
fn direction(icon: IconName, color: u32, value: String, cx: &App) -> Div {
    h_flex()
        .gap_1()
        .items_center()
        .child(Icon::new(icon).small().text_color(hex(color)))
        .child(div().text_sm().font_semibold().child(value))
        .text_color(cx.theme().foreground)
}

fn traffic_chart(history: &VecDeque<Pair>, cx: &App) -> AnyElement {
    if history.len() < 2 {
        return muted("Measuring...", cx).into_any_element();
    }
    let max = history
        .iter()
        .fold(1.0_f64, |max, (down, up)| max.max(*down).max(*up));
    let len = history.len();
    let points: Vec<(SharedString, Pair)> = history
        .iter()
        .enumerate()
        .map(|(i, pair)| (format!("-{}s", len - 1 - i).into(), *pair))
        .collect();
    let chart = AreaChart::new(points)
        .x(|(label, _): &(SharedString, Pair)| label.clone())
        .y_domain(0.0, max * 1.1)
        .x_axis(false)
        .grid(false)
        .appear(false)
        .y(|(_, pair): &(SharedString, Pair)| pair.0)
        .linear()
        .stroke(hex(DOWN_COLOR))
        .fill(hex(DOWN_COLOR).opacity(0.1))
        .name("Download")
        .y(|(_, pair): &(SharedString, Pair)| pair.1)
        .linear()
        .stroke(hex(UP_COLOR))
        .fill(hex(UP_COLOR).opacity(0.1))
        .name("Upload");
    div().h(px(120.)).child(chart).into_any_element()
}

impl HubApp {
    /// Polls adapters off the UI thread, one request at a time.
    fn refresh_network(&mut self, cx: &mut Context<Self>) {
        let page = &mut self.system.network;
        if page.loading || page.sampled.is_some_and(|at| at.elapsed() < REFRESH) {
            return;
        }
        page.loading = true;
        cx.spawn(async move |this, cx| {
            let adapters = cx.background_executor().spawn(async { net::list() }).await;
            let _ = this.update(cx, |app, cx| {
                app.system.network.apply(adapters);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn render_network(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.refresh_network(cx);
        let page = &self.system.network;
        let header = page_header(IconName::Wifi, "Network", "Adapters and live traffic", cx);
        let mut adapters = match &page.adapters {
            Some(Ok(adapters)) => adapters.clone(),
            Some(Err(err)) => {
                return v_flex()
                    .gap_4()
                    .child(header)
                    .child(empty_state(
                        IconName::Network,
                        "Network unavailable",
                        err.clone(),
                        cx,
                    ))
                    .into_any_element();
            }
            None => Vec::new(),
        };
        // Connected adapters first, then by name.
        adapters.sort_by(|a, b| b.up.cmp(&a.up).then_with(|| a.name.cmp(&b.name)));

        let (down, up) = page.total_rate();
        let session = adapters
            .iter()
            .map(|adapter| page.session_bytes(adapter))
            .fold((0., 0.), |sum, pair| (sum.0 + pair.0, sum.1 + pair.1));
        let traffic = section(IconName::Activity, "Traffic", cx)
            .child(
                h_flex()
                    .gap_6()
                    .child(direction(
                        IconName::ArrowDown,
                        DOWN_COLOR,
                        format_rate(down),
                        cx,
                    ))
                    .child(direction(IconName::ArrowUp, UP_COLOR, format_rate(up), cx)),
            )
            .child(traffic_chart(&page.history, cx))
            .child(stat_line(
                "This session",
                format!(
                    "{} received, {} sent",
                    format_bytes(session.0),
                    format_bytes(session.1)
                ),
                cx,
            ));

        let mut list = v_flex().gap_4().child(header).child(traffic);
        if adapters.is_empty() {
            list = list.child(empty_state(
                IconName::Network,
                "No network adapter",
                "No adapter was found.",
                cx,
            ));
        }
        for adapter in &adapters {
            list = list.child(adapter_card(adapter, page, cx));
        }
        list.into_any_element()
    }
}

fn adapter_card(adapter: &Adapter, page: &NetworkPage, cx: &App) -> Div {
    let icon = if adapter.wireless {
        IconName::Wifi
    } else {
        IconName::Network
    };
    let status = if adapter.up {
        solid_tag(GREEN).small().child("Connected")
    } else {
        Tag::secondary().outline().small().child("Disconnected")
    };
    let mut card = section(icon, adapter.name.clone(), cx).child(
        h_flex()
            .justify_between()
            .gap_2()
            .child(muted(adapter.description.clone(), cx))
            .child(status),
    );
    if !adapter.up {
        return card;
    }
    let join = |v4: bool| {
        adapter
            .addresses
            .iter()
            .filter(|ip| ip.is_ipv4() == v4)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let (ipv4, ipv6) = (join(true), join(false));
    let (down, up) = page.rates.get(&adapter.luid).copied().unwrap_or_default();
    let (received, sent) = page.session_bytes(adapter);
    card = card
        .child(
            h_flex()
                .gap_6()
                .child(direction(
                    IconName::ArrowDown,
                    DOWN_COLOR,
                    format_rate(down),
                    cx,
                ))
                .child(direction(IconName::ArrowUp, UP_COLOR, format_rate(up), cx)),
        )
        .when(!ipv4.is_empty(), |card| {
            card.child(stat_line("IPv4", ipv4, cx))
        })
        .when(!ipv6.is_empty(), |card| {
            card.child(stat_line("IPv6", ipv6, cx))
        })
        .when(!adapter.mac.is_empty(), |card| {
            card.child(stat_line("MAC address", adapter.mac.clone(), cx))
        })
        .child(stat_line(
            "Link speed",
            format_link_speed(adapter.link_speed),
            cx,
        ))
        .child(stat_line(
            "This session",
            format!(
                "{} received, {} sent",
                format_bytes(received),
                format_bytes(sent)
            ),
            cx,
        ));
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(luid: u64, rx_bytes: u64, tx_bytes: u64) -> Adapter {
        Adapter {
            luid,
            name: format!("eth{luid}"),
            description: String::new(),
            wireless: false,
            up: true,
            addresses: Vec::new(),
            mac: String::new(),
            link_speed: None,
            rx_bytes,
            tx_bytes,
        }
    }

    #[test]
    fn rates_history_and_session_totals() {
        let mut page = NetworkPage::default();
        page.sample(&[adapter(1, 1_000, 500), adapter(2, 0, 0)], None);
        assert!(page.rates.is_empty() && page.history.is_empty());

        page.sample(&[adapter(1, 3_000, 1_500), adapter(2, 1_000, 0)], Some(2.0));
        assert_eq!(page.rates[&1], (1_000., 500.));
        assert_eq!(page.history.back(), Some(&(1_500., 500.)));
        assert_eq!(
            page.session_bytes(&adapter(1, 3_000, 1_500)),
            (2_000., 1_000.)
        );

        // Long pause: no rate, no history point, totals still counted.
        page.sample(&[adapter(1, 9_000, 1_500)], Some(30.0));
        assert!(page.rates.is_empty());
        assert_eq!(page.history.len(), 1);
        assert_eq!(
            page.session_bytes(&adapter(1, 9_000, 1_500)),
            (8_000., 1_000.)
        );
    }

    #[test]
    fn history_is_bounded() {
        let mut page = NetworkPage::default();
        for i in 0..=HISTORY as u64 + 5 {
            page.sample(&[adapter(1, i * 100, 0)], Some(1.0));
        }
        assert_eq!(page.history.len(), HISTORY);
    }
}
