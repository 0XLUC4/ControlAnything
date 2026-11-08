# ControlAnything sous Linux (systemd)

Le daemon `controlanythingd` tourne en **root** : il écrit dans `/sys/class/hwmon/*/pwm*` et sa config
vit dans `/etc/controlanything/config.toml` (créée au premier lancement).

## Prérequis capteurs

- `lm-sensors`, puis `sudo sensors-detect` (accepter les modules proposés) et vérifier avec `sensors`.
- Cartes MSI (Super I/O **NCT6687D**) : le noyau ne la gère pas en natif, installer le module
  DKMS `nct6687d` (https://github.com/Fred78290/nct6687d), puis `sudo modprobe nct6687` et
  `echo nct6687 | sudo tee /etc/modules-load.d/nct6687.conf` pour le charger au boot.

## Installation

```sh
cargo build --release -p hub-daemon
sudo install -m 755 target/release/controlanythingd /usr/bin/controlanythingd
sudo install -m 644 packaging/linux/controlanythingd.service /etc/systemd/system/controlanythingd.service
sudo systemctl daemon-reload
sudo systemctl enable --now controlanythingd
```

Logs : `journalctl -u controlanythingd -f`. Arrêt : `sudo systemctl stop controlanythingd` (ventilos rendus au BIOS).

## Désinstallation

```sh
sudo systemctl disable --now controlanythingd
sudo rm /etc/systemd/system/controlanythingd.service /usr/bin/controlanythingd
sudo systemctl daemon-reload
```
