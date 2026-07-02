#/bin/env sh

systemctl stop easytier-core
mount -o remount,rw /

cd /lib/systemd/system/
cat << EOF > easytier-core.service
[Unit]
Description=easytier-core.service
After=network.target port_bridge.service dnsmasq.service lighttpd.service

[Service]
ExecStart=/usr/sbin/easytier-core -d --network-name w3cgame --network-secret w3cgame -p tcp://8.153.74.57:11010 --dev-name tailscale0
ExecStop=/usr/bin/kill -s HUP \$MAINPID
Restart=on-failure
RestartSec=60s

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
ln -sf "/lib/systemd/system/easytier-core.service" "/lib/systemd/system/multi-user.target.wants/"
mount -o remount,ro /
systemctl start easytier-core

