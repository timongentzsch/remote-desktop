#!/bin/sh
# install.sh itself, mounted as /install.sh, with a folder standing in for the release: what it refuses before
# it downloads anything, and that it gets the program run where the temporary folder runs nothing, which takes
# --tmpfs /nx:noexec.
# sudo, modprobe and udevadm are stand-ins as in install-test.sh, and curl is one that copies.
mkdir -p /fake /release /etc/udev/rules.d
for tool in modprobe udevadm; do printf '#!/bin/sh\n' > /fake/$tool; done
printf '#!/bin/sh\ncp "${4#file://}" "$3"\n' > /fake/curl # as it is called: -fsSL -o FILE ADDRESS
cp ./remote-desktop /release/remote-desktop-$(uname -m)
chmod +x /fake/*
export REMOTE_DESKTOP_RELEASE=file:///release
script() { PATH=/fake:$PATH sh /install.sh --no-password 2>&1 | grep "^remote-desktop:\|^Installed\|not found\|denied" | head -1 | cut -c1-110; }
echo "no sudo: $(script)"
printf '#!/bin/sh\nexec "$@"\n' > /fake/sudo; chmod +x /fake/sudo
answers() { printf '#!/bin/sh\ncase $1 in GNU_LIBC_VERSION) echo "glibc %s" ;; *) echo %s ;; esac\n' "$1" "$2" > /fake/getconf; chmod +x /fake/getconf; }
answers 2.27 64; echo "an old glibc: $(script)"
answers 2.41 32; echo "a 32-bit system: $(script)"
rm /fake/getconf
echo "as it is: $(script)"
/usr/local/bin/remote-desktop uninstall >/dev/null 2>&1
printf '#!/bin/sh\n' > /nx/runs; chmod +x /nx/runs
echo "a temporary folder that runs nothing ($(/nx/runs 2>/dev/null && echo "but this one does" || echo "as this one")):"
echo "  $(TMPDIR=/nx script); left in the cache: $(ls $HOME/.cache 2>/dev/null | grep -c remote-desktop)"
/usr/local/bin/remote-desktop uninstall >/dev/null 2>&1
