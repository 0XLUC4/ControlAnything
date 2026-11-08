#!/bin/sh
# Builds target/packager/controlanything_<ver>_<arch>.deb with dpkg-deb (no extra tool needed).
# cargo-packager 0.11 has no deb maintainer scripts (postinst/prerm), hence this script.
# Usage (from the repo root, on Linux): sh packaging/linux/build-deb.sh
set -eu
cd "$(dirname "$0")/../.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
arch=$(dpkg --print-architecture)
cargo build --release -p hub-app -p hub-daemon

root=target/packager/deb-root
rm -rf "$root"
install -Dm755 target/release/controlanything  "$root/usr/bin/controlanything"
install -Dm755 target/release/controlanythingd "$root/usr/bin/controlanythingd"
install -Dm644 packaging/linux/controlanythingd.service "$root/lib/systemd/system/controlanythingd.service"
install -Dm644 packaging/linux/controlanything.desktop  "$root/usr/share/applications/controlanything.desktop"
icon=assets/icons/controlanything.png
[ -f "$icon" ] || icon=assets/icons/fan.png # shortcut: fallback until a PNG app icon exists
install -Dm644 "$icon" "$root/usr/share/pixmaps/controlanything.png"
install -Dm644 README.md "$root/usr/share/doc/controlanything/README.md"
install -Dm755 -t "$root/DEBIAN" packaging/linux/debian/postinst packaging/linux/debian/prerm packaging/linux/debian/postrm
cat > "$root/DEBIAN/control" <<CONTROL
Package: controlanything
Version: $version
Architecture: $arch
Maintainer: ControlAnything
Section: utils
Priority: optional
Depends: systemd
Recommends: lm-sensors
Description: ControlAnything - capteurs, ventilateurs et RGB du PC
 Daemon systemd (root) controlanythingd + interface graphique controlanything.
CONTROL

out="target/packager/controlanything_${version}_${arch}.deb"
dpkg-deb --root-owner-group --build "$root" "$out"
echo "Paquet : $out"
