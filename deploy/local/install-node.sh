#!/bin/sh
# Install an unpacked node release. Configuration is provisioned separately.
set -eu
release=${1:?Usage: install-node.sh ABSOLUTE_RELEASE_DIRECTORY}
case "$release" in /opt/wisp/releases/*) ;; *) echo 'Release must be under /opt/wisp/releases' >&2; exit 1;; esac
cd "$release"
sha256sum --check SHA256SUMS
if ! getent passwd wisp >/dev/null; then
    useradd --system --home-dir /var/lib/wisp --shell /usr/sbin/nologin wisp
fi
install -d -o wisp -g wisp -m 0700 /var/lib/wisp
install -d -o root -g wisp -m 0750 /etc/wisp
install -m 0644 wispd.service /etc/systemd/system/wispd.service
ln -sfn "$release" /opt/wisp/current
systemctl daemon-reload
systemctl enable wispd.service
