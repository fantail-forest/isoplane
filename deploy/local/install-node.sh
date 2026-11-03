#!/bin/sh
# Install an unpacked node release. Configuration is provisioned separately.
set -eu
release=${1:?Usage: install-node.sh ABSOLUTE_RELEASE_DIRECTORY}
case "$release" in /opt/isoplane/releases/*) ;; *) echo 'Release must be under /opt/isoplane/releases' >&2; exit 1;; esac
cd "$release"
sha256sum --check SHA256SUMS
install -d -o root -g root -m 0755 /opt/isoplane /opt/isoplane/releases
chown -R root:root "$release"
chmod -R u=rwX,go=rX "$release"
chmod 0755 isoplaned install-node.sh
if ! getent passwd isoplane >/dev/null; then
    useradd --system --home-dir /var/lib/isoplane --shell /usr/sbin/nologin isoplane
fi
install -d -o isoplane -g isoplane -m 0700 /var/lib/isoplane
install -d -o root -g isoplane -m 0750 /etc/isoplane
install -m 0644 isoplaned.service /etc/systemd/system/isoplaned.service
ln -sfn "$release" /opt/isoplane/current
install -d -m 0755 /usr/local/bin
ln -sfn /opt/isoplane/current/isoplaned /usr/local/bin/isoplaned
systemctl daemon-reload
systemctl enable isoplaned.service
