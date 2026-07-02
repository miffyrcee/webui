#/bin/env sh

systemctl stop webui
mount -o remount,rw /

cd /lib/systemd/system/
cat << EOF > webui.service
[Unit]
Description=webui.service
After=network.target port_bridge.service dnsmasq.service lighttpd.service

[Service]
ExecStart=/opt/bin/quectel-webui
ExecStop=/usr/bin/kill -s HUP \$MAINPID
Restart=on-failure
RestartSec=60s

[Install]
WantedBy=multi-user.target
EOF

ln -sf "/lib/systemd/system/webui.service" "/lib/systemd/system/multi-user.target.wants/"
systemctl daemon-reload
mount -o remount,ro /
systemctl start webui