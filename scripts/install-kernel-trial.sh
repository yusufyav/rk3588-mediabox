#!/bin/sh
# Installs a trial kernel next to the running one and arms a one-shot restore.
# Usage: install-trial.sh <release> <tarball>   (run on the board as root)
set -eu
rel=$1
tarball=$2
old=6.1.115-vendor-rk35xx
trial=/var/tmp/mbx-kernel-trial-$rel

[ "$(tr -d '\000' </proc/device-tree/model)" = 'Orange Pi 5 Plus' ]
[ "$(uname -r)" = "$old" ]
[ -f "/boot/vmlinuz-$old" ] && [ -f "/boot/uInitrd-$old" ] && [ -d "/boot/dtb-$old" ]
[ ! -e "/lib/modules/$rel" ] || { echo "/lib/modules/$rel exists; refusing"; exit 1; }

mkdir -p "$trial"
ls -l /boot/Image /boot/uInitrd /boot/dtb > "$trial/links.before"

tmp=$(mktemp -d)
tar -xzf "$tarball" -C "$tmp"
install -m 0644 "$tmp/boot/vmlinuz-$rel" "/boot/vmlinuz-$rel"
install -m 0644 "$tmp/boot/config-$rel" "/boot/config-$rel"
install -m 0644 "$tmp/boot/System.map-$rel" "/boot/System.map-$rel"
cp -a "$tmp/lib/modules/$rel" "/lib/modules/$rel"
rm -rf "$tmp"
depmod -a "$rel"

# Armbian's post-update hook writes /boot/uInitrd-$rel and repoints
# /boot/uInitrd; the latter is put back below until the trial is armed.
update-initramfs -c -k "$rel"
[ -f "/boot/uInitrd-$rel" ] || mkimage -A arm64 -O linux -T ramdisk -C gzip -n uInitrd \
	-d "/boot/initrd.img-$rel" "/boot/uInitrd-$rel"
ln -sfn "uInitrd-$old" /boot/uInitrd

cat > "$trial/restore-kernel-links.sh" <<EOF
#!/bin/sh
set -eu
ln -sfn "vmlinuz-$old" /boot/Image
ln -sfn "uInitrd-$old" /boot/uInitrd
ln -sfn "dtb-$old" /boot/dtb
sync
printf '%s next boot restored to %s; running %s\n' "\$(date -Is)" "$old" "\$(uname -r)" >> "$trial/restore.log"
rm -f "$trial/armed"
EOF
chmod 0755 "$trial/restore-kernel-links.sh"

cat > /etc/systemd/system/mediabox-kernel-trial-restore.service <<EOF
[Unit]
Description=Restore the original kernel selection after the trial kernel $rel boots
After=local-fs.target
ConditionKernelVersion=$rel
ConditionPathExists=$trial/armed

[Service]
Type=oneshot
ExecStart=$trial/restore-kernel-links.sh

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable mediabox-kernel-trial-restore.service

# Arm: the next boot runs the trial kernel, once.
touch "$trial/armed"
ln -sfn "vmlinuz-$rel" /boot/Image
ln -sfn "uInitrd-$rel" /boot/uInitrd
sync
ls -l /boot/Image /boot/uInitrd /boot/dtb | tee "$trial/links.armed"
echo "armed: next boot runs $rel once"
