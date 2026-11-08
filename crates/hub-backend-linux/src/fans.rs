//! Fan control through sysfs hwmon (`pwmN` + `pwmN_enable`).

use crate::{HWMON_ROOT, list_chips, read_trimmed};
use hub_core::HwError;
use hub_core::fan::{FanChannel, FanController};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Discovered PWM output, with the original state saved on first touch.
#[derive(Debug)]
struct Pwm {
    pwm: PathBuf,
    enable: PathBuf,
    /// Original (enable, pwm); `None` until the channel is touched.
    saved: Option<(String, String)>,
}

/// hwmon-based fan controller. Hands control back to the firmware on drop.
#[derive(Debug)]
pub struct HwmonFans {
    channels: Vec<FanChannel>,
    /// Parallel to `channels` (same index).
    pwms: Vec<Pwm>,
}

impl HwmonFans {
    /// Scans `/sys/class/hwmon`.
    pub fn discover() -> Result<Self, HwError> {
        Self::discover_in(Path::new(HWMON_ROOT))
    }

    /// Same, from an arbitrary directory (used by tests).
    pub fn discover_in(root: &Path) -> Result<Self, HwError> {
        let mut fans = Self {
            channels: Vec::new(),
            pwms: Vec::new(),
        };
        for (dir, _name, chip) in list_chips(root)? {
            let mut indices: Vec<u32> = fs::read_dir(&dir)?
                .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                .filter_map(|file| file.strip_prefix("pwm")?.parse().ok())
                .filter(|n| dir.join(format!("pwm{n}_enable")).is_file())
                .collect();
            indices.sort_unstable();
            for n in indices {
                let label = read_trimmed(&dir.join(format!("pwm{n}_label")))
                    .unwrap_or_else(|| format!("{chip} PWM {n}"));
                let rpm_sensor = dir
                    .join(format!("fan{n}_input"))
                    .is_file()
                    .then(|| format!("{chip}/fan{n}"));
                fans.channels.push(FanChannel {
                    id: format!("{chip}/pwm{n}"),
                    label,
                    rpm_sensor,
                });
                fans.pwms.push(Pwm {
                    pwm: dir.join(format!("pwm{n}")),
                    enable: dir.join(format!("pwm{n}_enable")),
                    saved: None,
                });
            }
        }
        Ok(fans)
    }

    fn pwm_mut(&mut self, channel: &str) -> Result<&mut Pwm, HwError> {
        let index = self
            .channels
            .iter()
            .position(|c| c.id == channel)
            .ok_or_else(|| HwError::Other(format!("unknown channel: {channel}")))?;
        Ok(&mut self.pwms[index])
    }
}

impl FanController for HwmonFans {
    fn name(&self) -> &str {
        "hwmon"
    }

    fn channels(&self) -> &[FanChannel] {
        &self.channels
    }

    fn set_duty(&mut self, channel: &str, percent: u8) -> Result<(), HwError> {
        let pwm = self.pwm_mut(channel)?;
        if pwm.saved.is_none() {
            pwm.saved = Some((read(&pwm.enable)?, read(&pwm.pwm)?));
        }
        if read(&pwm.enable)? != "1" {
            write(&pwm.enable, "1")?;
        }
        let raw = (u32::from(percent.min(100)) * 255 + 50) / 100;
        write(&pwm.pwm, &raw.to_string())
    }

    fn set_auto(&mut self, channel: &str) -> Result<(), HwError> {
        let pwm = self.pwm_mut(channel)?;
        let Some((enable, duty)) = pwm.saved.take() else {
            return Ok(());
        };
        write(&pwm.enable, &enable)?;
        // Channel was originally manual: restore its PWM value too.
        if enable == "1" {
            write(&pwm.pwm, &duty)?;
        }
        Ok(())
    }

    fn restore_all(&mut self) {
        let touched: Vec<String> = self
            .channels
            .iter()
            .zip(&self.pwms)
            .filter(|(_, pwm)| pwm.saved.is_some())
            .map(|(channel, _)| channel.id.clone())
            .collect();
        for id in touched {
            let _ = self.set_auto(&id);
        }
    }
}

impl Drop for HwmonFans {
    fn drop(&mut self) {
        self.restore_all();
    }
}

fn read(path: &Path) -> Result<String, HwError> {
    fs::read_to_string(path)
        .map(|text| text.trim().to_owned())
        .map_err(|err| io_error(path, err))
}

fn write(path: &Path, value: &str) -> Result<(), HwError> {
    fs::write(path, value).map_err(|err| io_error(path, err))
}

fn io_error(path: &Path, err: io::Error) -> HwError {
    if err.kind() == io::ErrorKind::PermissionDenied {
        HwError::PermissionDenied(format!("{} (root or udev rule required)", path.display()))
    } else {
        HwError::Io(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fake sysfs tree: an nct6687 chip (pwm1 auto + fan1, pwm2 manual) and an amdgpu.
    struct FakeSysfs(PathBuf);

    impl FakeSysfs {
        fn new(test: &str) -> Self {
            let root = std::env::temp_dir().join(format!("hub-fans-{}-{test}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            for (rel, content) in [
                ("hwmon0/name", "nct6687\n"),
                ("hwmon0/pwm1", "90\n"),
                ("hwmon0/pwm1_enable", "2\n"),
                ("hwmon0/fan1_input", "1200\n"),
                ("hwmon0/pwm2", "40\n"),
                ("hwmon0/pwm2_enable", "1\n"),
                ("hwmon0/pwm3", "0\n"), // no _enable: ignored
                ("hwmon1/name", "amdgpu\n"),
                ("hwmon1/pwm1", "60\n"),
                ("hwmon1/pwm1_enable", "2\n"),
                ("hwmon2/name", "nct6687\n"),
                ("hwmon2/pwm1", "10\n"),
                ("hwmon2/pwm1_enable", "2\n"),
            ] {
                let path = root.join(rel);
                fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
                fs::write(path, content).expect("write");
            }
            Self(root)
        }

        fn get(&self, rel: &str) -> String {
            fs::read_to_string(self.0.join(rel))
                .expect("read")
                .trim()
                .to_owned()
        }

        fn fans(&self) -> HwmonFans {
            HwmonFans::discover_in(&self.0).expect("discover")
        }
    }

    impl Drop for FakeSysfs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn discovers_channels_with_rpm_mapping() {
        let sysfs = FakeSysfs::new("discover");
        let fans = sysfs.fans();
        let ids: Vec<&str> = fans.channels().iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "nct6687/pwm1",
                "nct6687/pwm2",
                "amdgpu/pwm1",
                "nct6687#2/pwm1"
            ]
        );
        assert_eq!(fans.channels()[0].label, "nct6687 PWM 1");
        assert_eq!(
            fans.channels()[0].rpm_sensor.as_deref(),
            Some("nct6687/fan1")
        );
        assert_eq!(fans.channels()[1].rpm_sensor, None);
    }

    #[test]
    fn set_duty_switches_to_manual_and_scales() {
        let sysfs = FakeSysfs::new("duty");
        let mut fans = sysfs.fans();
        for (percent, raw) in [(0, "0"), (50, "128"), (100, "255"), (200, "255")] {
            fans.set_duty("nct6687/pwm1", percent).expect("set_duty");
            assert_eq!(sysfs.get("hwmon0/pwm1_enable"), "1");
            assert_eq!(sysfs.get("hwmon0/pwm1"), raw);
        }
    }

    #[test]
    fn set_auto_restores_originals() {
        let sysfs = FakeSysfs::new("auto");
        let mut fans = sysfs.fans();
        fans.set_auto("nct6687/pwm1")
            .expect("no-op if never touched");
        fans.set_duty("nct6687/pwm1", 100).expect("duty");
        fans.set_duty("nct6687/pwm2", 100).expect("duty");
        fans.set_auto("nct6687/pwm1").expect("auto");
        fans.set_auto("nct6687/pwm2").expect("auto");
        assert_eq!(sysfs.get("hwmon0/pwm1_enable"), "2");
        // Originally manual channel: its PWM value comes back too.
        assert_eq!(sysfs.get("hwmon0/pwm2_enable"), "1");
        assert_eq!(sysfs.get("hwmon0/pwm2"), "40");
    }

    #[test]
    fn drop_restores_touched_channels() {
        let sysfs = FakeSysfs::new("drop");
        {
            let mut fans = sysfs.fans();
            fans.set_duty("amdgpu/pwm1", 30).expect("duty");
            fans.set_duty("nct6687/pwm2", 30).expect("duty");
        }
        assert_eq!(sysfs.get("hwmon1/pwm1_enable"), "2");
        assert_eq!(sysfs.get("hwmon0/pwm2"), "40");
    }

    #[test]
    fn unknown_channel_is_an_error() {
        let sysfs = FakeSysfs::new("unknown");
        let mut fans = sysfs.fans();
        assert!(matches!(
            fans.set_duty("nope/pwm9", 50),
            Err(HwError::Other(_))
        ));
        assert!(matches!(fans.set_auto("nope/pwm9"), Err(HwError::Other(_))));
    }
}
