//! Notifications système : surchauffe (failsafe) et arrêt du daemon.

use std::time::{Duration, Instant};

/// Silence du daemon au-delà duquel on prévient l'utilisateur.
const DAEMON_LOST_AFTER: Duration = Duration::from_secs(10);

/// Détecte les débuts d'épisode : une notification par épisode, pas à chaque message.
#[derive(Default)]
pub struct Alerts {
    failsafe: bool,
    daemon_lost: bool,
}

impl Alerts {
    /// `true` au passage false → true du failsafe.
    pub fn failsafe_rising(&mut self, failsafe: bool) -> bool {
        let rising = failsafe && !self.failsafe;
        self.failsafe = failsafe;
        rising
    }

    /// `true` une seule fois quand le daemon, déjà vu, se tait depuis plus de 10 s.
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

/// Affiche une notification système (thread dédié : D-Bus/WinRT peuvent bloquer).
pub fn notify(summary: &'static str, body: &'static str) {
    std::thread::spawn(move || {
        let shown = notify_rust::Notification::new()
            .appname("ControlAnything")
            .summary(summary)
            .body(body)
            .show();
        if let Err(err) = shown {
            tracing::warn!("notification impossible : {err}");
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
        // Reconnexion puis nouvelle perte : nouvelle notification.
        assert!(!alerts.daemon_lost(Some(later(30)), later(31)));
        assert!(alerts.daemon_lost(Some(later(30)), later(45)));
    }
}
