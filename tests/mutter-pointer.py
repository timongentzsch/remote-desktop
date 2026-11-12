"""Moves a headless Mutter's pointer back and forth for some seconds, through Mutter's own remote-desktop
service: a headless Mutter has no pointer until something moves one. The session lasts as long as this runs.
Argument: seconds."""
import sys, time
from gi.repository import Gio, GLib

bus = Gio.bus_get_sync(Gio.BusType.SESSION)


def call(path, interface, method, arguments=None):
    return bus.call_sync("org.gnome.Mutter.RemoteDesktop", path, interface, method, arguments, None, 0, 5000, None)


session = call("/org/gnome/Mutter/RemoteDesktop", "org.gnome.Mutter.RemoteDesktop", "CreateSession").unpack()[0]
call(session, "org.gnome.Mutter.RemoteDesktop.Session", "Start")
end, step = time.time() + float(sys.argv[1]), 1
while time.time() < end:
    move = GLib.Variant("(dd)", (40.0 * step, 30.0 * step))
    call(session, "org.gnome.Mutter.RemoteDesktop.Session", "NotifyPointerMotionRelative", move)
    step = -step
    time.sleep(0.5)
