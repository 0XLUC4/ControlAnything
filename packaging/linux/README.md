# ControlAnything on Linux (systemd)

The `controlanythingd` daemon runs as **root**: it writes to `/sys/class/hwmon/*/pwm*` and its config
lives in `/etc/controlanything/config.toml` (created on first launch).

## Sensor requirements

- `lm-sensors`, then `sudo sensors-detect` (accept the suggested modules) and check with `sensors`.
- MSI boards (**NCT6687D** Super I/O): the kernel does not support it natively, install the
  `nct6687d` DKMS module (https://github.com/Fred78290/nct6687d), then `sudo modprobe nct6687` and
  `echo nct6687 | sudo tee /etc/modules-load.d/nct6687.conf` to load it at boot.

## Installation

```sh
cargo build --release -p hub-daemon
sudo install -m 755 target/release/controlanythingd /usr/bin/controlanythingd
sudo install -m 644 packaging/linux/controlanythingd.service /etc/systemd/system/controlanythingd.service
sudo systemctl daemon-reload
sudo systemctl enable --now controlanythingd
```

Logs: `journalctl -u controlanythingd -f`. Stop: `sudo systemctl stop controlanythingd` (fans handed back to the BIOS).

## Uninstall

```sh
sudo systemctl disable --now controlanythingd
sudo rm /etc/systemd/system/controlanythingd.service /usr/bin/controlanythingd
sudo systemctl daemon-reload
```
