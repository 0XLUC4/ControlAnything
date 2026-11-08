//! System notifications: overheating (failsafe), daemon shutdown and user thresholds.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use hub_core::Reading;

use crate::prefs::Prefs;

/// Daemon silence after which the user is warned.
const DAEMON_LOST_AFTER: Duration = Duration::from_secs(10);
/// Minimum delay between two notifications of the same user threshold.
const ALARM_COOLDOWN: Duration = Duration::from_secs(300);

/// Detects episode starts: one notification per episode, not one per message.
#[derive(Default)]
pub struct Alerts {
    failsafe: bool,
    daemon_lost: bool,
    /// Last notification of each user threshold, by sensor id.
    alarm_fired: HashMap<String, Instant>,
}

impl Alerts {
    /// `true` when the failsafe goes from false to true.
    pub fn failsafe_rising(&mut self, failsafe: bool) -> bool {
        let rising = failsafe && !self.failsafe;
        self.failsafe = failsafe;
        rising
    }

    /// `true` only once when the daemon, already seen, has been silent for more than 10 s.
    pub fn daemon_lost(&mut self, last_update: Option<Instant>, now: Instant) -> bool {
        let Some(at) = last_update else {
            return false;
        };
        let lost = now.saturating_duration_since(at) > DAEMON_LOST_AFTER;
        let first = lost && !self.daemon_lost;
        self.daemon_lost = lost;
        first
    }
}

impl Alerts {
    /// `true` when a breached threshold may notify: once per sensor every 5 min.
    fn alarm_due(&mut self, id: &str, breached: bool, now: Instant) -> bool {
        if !breached {
            return false;
        }
        let due = self
            .alarm_fired
            .get(id)
            .is_none_or(|at| now.saturating_duration_since(*at) >= ALARM_COOLDOWN);
        if due {
            self.alarm_fired.insert(id.to_owned(), now);
        }
        due
    }

    /// Notifications (summary, body) for user thresholds crossed by these readings.
    pub fn check_thresholds(
        &mut self,
        readings: &[Reading],
        prefs: &Prefs,
        now: Instant,
    ) -> Vec<(String, String)> {
        let mut fired = Vec::new();
        for reading in readings {
            let unit = prefs.temperature_unit;
            if let Some(&limit) = prefs.temp_alarms.get(&reading.id)
                && self.alarm_due(&reading.id, reading.value > limit, now)
            {
                fired.push((
                    format!("{} is hot", reading.label),
                    format!(
                        "{} (alert above {}).",
                        unit.format(reading.value, 0),
                        unit.format(limit, 0)
                    ),
                ));
            }
            if let Some(&limit) = prefs.rpm_alarms.get(&reading.id)
                && self.alarm_due(&reading.id, reading.value < limit, now)
            {
                fired.push((
                    format!("{} may be stalled", reading.label),
                    format!("{:.0} RPM (alert below {limit:.0} RPM).", reading.value),
                ));
            }
        }
        fired
    }
}

/// Shows a system notification (dedicated thread: D-Bus/WinRT may block).
pub fn notify(summary: impl Into<String>, body: impl Into<String>) {
    let (summary, body) = (summary.into(), body.into());
    std::thread::spawn(move || {
        let shown = notify_rust::Notification::new()
            .appname("ControlAnything")
            .summary(&summary)
            .body(&body)
            .show();
        if let Err(err) = shown {
            tracing::warn!("cannot show notification: {err}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failsafe_notifies_once_per_episode() {
        let mut alerts = Alerts::default();
        assert!(!alerts.failsafe_rising(false));
        assert!(alerts.failsafe_rising(true));
        assert!(!alerts.failsafe_rising(true));
        assert!(!alerts.failsafe_rising(false));
        assert!(alerts.failsafe_rising(true));
    }

    #[test]
    fn daemon_loss_needs_previous_connection_and_10_s() {
        let mut alerts = Alerts::default();
        let start = Instant::now();
        let later = |s| start + Duration::from_secs(s);
        assert!(!alerts.daemon_lost(None, later(60)));
        assert!(!alerts.daemon_lost(Some(start), later(5)));
        assert!(alerts.daemon_lost(Some(start), later(11)));
        assert!(!alerts.daemon_lost(Some(start), later(20)));
        // Reconnection then a new loss: new notification.
        assert!(!alerts.daemon_lost(Some(later(30)), later(31)));
        assert!(alerts.daemon_lost(Some(later(30)), later(45)));
    }

    #[test]
    fn thresholds_fire_with_cooldown() {
        use hub_core::{Device, SensorKind};
        let mut prefs = Prefs::default();
        prefs.temp_alarms.insert("cpu".into(), 80.0);
        prefs.rpm_alarms.insert("fan1".into(), 300.0);
        let reading = |id: &str, kind, value| Reading {
            id: id.into(),
            label: id.into(),
            device: Device::Cpu,
            kind,
            value,
        };
        let hot = [
            reading("cpu", SensorKind::Temperature, 90.0),
            reading("fan1", SensorKind::Fan, 0.0),
        ];
        let cool = [
            reading("cpu", SensorKind::Temperature, 50.0),
            reading("fan1", SensorKind::Fan, 900.0),
        ];
        let mut alerts = Alerts::default();
        let start = Instant::now();
        let later = |s| start + Duration::from_secs(s);
        assert!(alerts.check_thresholds(&cool, &prefs, start).is_empty());
        assert_eq!(alerts.check_thresholds(&hot, &prefs, start).len(), 2);
        // Still breached within the cooldown: silent.
        assert!(alerts.check_thresholds(&hot, &prefs, later(60)).is_empty());
        assert_eq!(alerts.check_thresholds(&hot, &prefs, later(301)).len(), 2);
    }
}
