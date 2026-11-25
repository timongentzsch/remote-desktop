#!/bin/sh
# install and uninstall with a real ufw and a real firewalld, which takes --cap-add NET_ADMIN: the rule opens the
# host's port, for everyone or on Tailscale's interface, follows a change of port, replaces what versions before
# 0.10 opened for port 8765, and goes with uninstall. sudo, modprobe and udevadm are stand-ins as in install-test.sh.
mkdir -p $XDG_RUNTIME_DIR /fake /etc/udev/rules.d /run/dbus && chmod 700 $XDG_RUNTIME_DIR
printf '#!/bin/sh\nexec "$@"\n' > /fake/sudo
for tool in modprobe udevadm; do printf '#!/bin/sh\n' > /fake/$tool; done
chmod +x /fake/*; export PATH=/fake:$PATH
cp ./remote-desktop /tmp/remote-desktop
tailnet=$HOME/.config/remote-desktop/tailscale # names the tailnet certificate; with it the host is for the tailnet only
install() { /tmp/remote-desktop install --no-password "$@" >/dev/null || exit 1; }
expect() { [ "$2" = "$3" ] && echo "$1: $2" || { echo "FAILED, $1: '$2' instead of '$3'"; exit 1; }; }

sed -i 's/^IPV6=.*/IPV6=no/' /etc/default/ufw
ufw --force enable >/dev/null || exit 1
ufw allow 8765/tcp >/dev/null; ufw allow in on tailscale0 to any port 8765 proto udp >/dev/null
rules() { ufw show added | sed 1d | tr '\n' ';'; }
ports() { iptables -S ufw-user-input | grep -o "dport [0-9]*" | sort -u | tr '\n' ';'; }
install
expect "ufw, for everyone" "$(rules) $(ports)" "ufw allow remote-desktop; dport 443;"
echo name > $tailnet; install
expect "ufw, for the tailnet" "$(rules) $(ports)" "ufw allow in on tailscale0 to any app remote-desktop; dport 443;"
rm $tailnet; install --port=8765
expect "ufw, on port 8765" "$(rules) $(ports)" "ufw allow remote-desktop; dport 8765;"
/tmp/remote-desktop uninstall >/dev/null
expect "ufw, uninstalled" "$(rules) $(ls /etc/ufw/applications.d/remote-desktop 2>/dev/null)" "(None); "
ufw disable >/dev/null

dbus-daemon --system --fork
firewalld --nofork --nopid >/tmp/firewalld.log 2>&1 &
for try in $(seq 40); do firewall-cmd --state 2>/dev/null | grep -q running && break; sleep 0.5; done
firewall-cmd -q --permanent --add-port=8765/tcp --add-port=8765/udp
firewall-cmd -q --permanent --zone=trusted --add-interface=tailscale0
firewall-cmd -q --reload
zone() { echo "$(firewall-cmd --zone=$1 --list-services | tr ' ' '\n' | grep remote-desktop) ports: $(firewall-cmd --zone=$1 --list-ports)"; }
service() { firewall-cmd --permanent --service=remote-desktop --get-ports 2>/dev/null; }
install --port=443
expect "firewalld, for everyone" "$(zone public) / $(zone trusted) / $(service)" "remote-desktop ports:  /  ports:  / 443/tcp 443/udp"
echo name > $tailnet; install
expect "firewalld, for the tailnet" "$(zone public) / $(zone trusted)" " ports:  / remote-desktop ports: "
rm $tailnet; install --port=8765
expect "firewalld, on port 8765" "$(zone public) / $(zone trusted) / $(service)" "remote-desktop ports:  /  ports:  / 8765/tcp 8765/udp"
/tmp/remote-desktop uninstall >/dev/null
expect "firewalld, uninstalled" "$(zone public) / $(service) $(ls /etc/firewalld/services/remote-desktop.xml 2>/dev/null)" " ports:  /  "
