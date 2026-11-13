//! The login screen. A display manager shows it in a desktop session of its own, run by a user of its own,
//! before anyone has logged in. The host gets there by starting as root, waiting for such a session, and then
//! becoming that user in that session's environment: from then on it is the host as in any desktop session.
//! When the login screen goes, this process goes too, and the system service that runs it starts over.

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::{fail, server};

/// Where logind keeps a file for each session. A change there is what wakes the search for a login screen;
/// nothing is read from it.
const SESSIONS: &str = "/run/systemd/sessions";
/// What a display manager puts into the environment of its login screen's processes.
const GREETER: (&str, &str) = ("XDG_SESSION_CLASS", "greeter");
/// The variables by which a process finds the desktop session it runs in, a display among them.
const DISPLAYS: [&str; 2] = ["WAYLAND_DISPLAY", "DISPLAY"];
const DESKTOP: [&str; 5] = ["XDG_RUNTIME_DIR", "WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY", "DBUS_SESSION_BUS_ADDRESS"];
/// How often the login screen is looked for while its session is coming up, and for how long after a change.
const LOOK: Duration = Duration::from_millis(500);
const LOOKS: u32 = 30;

/// A login screen: one of its processes, the user it runs as, and where its desktop is.
struct Greeter {
    process: u32,
    user: u32,
    group: u32,
    desktop: Vec<(String, String)>,
}

/// Serves the login screen whenever there is one. Has to be started as root.
pub fn run(address: &str) -> ! {
    let greeter = wait();
    // What only root may: the owner's password and certificate, the input devices. Then root is given up.
    let host = server::Host::open();
    let listener = server::listen(address);
    if let Err(error) = greeter.become_its_user() {
        fail(&format!("cannot become the login screen's user: {error}"));
    }
    thread::spawn(move || {
        while greeter.is_there() {
            thread::sleep(Duration::from_secs(1));
        }
        std::process::exit(0) // someone logged in, or the display manager stopped
    });
    server::serve(listener, host)
}

/// The user of a session as logind describes it in its directory, if the session is a login screen's. Only
/// logind's word counts for whose processes are looked at: what a process has in its environment is its
/// owner's to say, and this service hands whoever it takes for the login screen the port and the password.
fn greeter_user(session: &str) -> Option<u32> {
    let value = |name: &str| session.lines().find_map(|line| line.strip_prefix(name)?.strip_prefix('='));
    let user = value("UID")?.parse().ok().filter(|&user| user != 0)?;
    (value("CLASS")? == GREETER.1).then_some(user)
}

/// Sleeps until there is a login screen.
fn wait() -> Greeter {
    let watch = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    if let Ok(path) = CString::new(SESSIONS) {
        let changes = libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_TO | libc::IN_CLOSE_WRITE;
        unsafe { libc::inotify_add_watch(watch, path.as_ptr(), changes) };
    }
    // A session shows up before its processes have their display, so after a change it is looked for some
    // times over. Where logind's directory cannot be watched, that goes on for good.
    let mut looks_left = LOOKS;
    loop {
        if let Some(greeter) = find() {
            unsafe { libc::close(watch) };
            return greeter;
        }
        let watching = Path::new(SESSIONS).is_dir() && watch >= 0;
        let patience = (looks_left > 0 || !watching).then_some(LOOK);
        if crate::fd::wait_readable([watch], patience) == [true] {
            let mut events = [0u8; 4096];
            unsafe { libc::read(watch, events.as_mut_ptr().cast(), events.len()) };
            looks_left = LOOKS;
        } else {
            looks_left = looks_left.saturating_sub(1);
        }
    }
}

/// The login screen, if a process says it belongs to one and knows a display. A Wayland one is preferred: a
/// login screen on Wayland has an X server as well, for old programs.
fn find() -> Option<Greeter> {
    let users: Vec<u32> = fs::read_dir(SESSIONS)
        .ok()?
        .flatten()
        .filter_map(|session| greeter_user(&fs::read_to_string(session.path()).ok()?))
        .collect();
    let mut found: Vec<Greeter> = fs::read_dir("/proc")
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let process = entry.file_name().to_str()?.parse().ok()?;
            let owner = entry.metadata().ok()?;
            let desktop =
                desktop_of(&fs::read(entry.path().join("environ")).ok().filter(|_| users.contains(&owner.uid()))?)?;
            Some(Greeter { process, user: owner.uid(), group: owner.gid(), desktop })
        })
        .collect();
    found.sort_by_key(|greeter| !greeter.desktop.iter().any(|(name, _)| name == DISPLAYS[0]));
    found.into_iter().next()
}

/// Where a process's desktop is, from its environment as the kernel shows it, if the process belongs to a
/// login screen and knows a display.
fn desktop_of(environment: &[u8]) -> Option<Vec<(String, String)>> {
    let variables: Vec<(&str, &str)> = environment
        .split(|&byte| byte == 0)
        .filter_map(|variable| std::str::from_utf8(variable).ok()?.split_once('='))
        .collect();
    let has = |wanted: &str| variables.iter().any(|(name, value)| *name == wanted && !value.is_empty());
    if !variables.contains(&GREETER) || !DISPLAYS.iter().any(|display| has(display)) {
        return None;
    }
    let kept = variables.iter().filter(|(name, _)| DESKTOP.contains(name));
    Some(kept.map(|(name, value)| (name.to_string(), value.to_string())).collect())
}

impl Greeter {
    /// Whether the process this was found by is still the login screen's.
    fn is_there(&self) -> bool {
        fs::metadata(format!("/proc/{}", self.process)).is_ok_and(|owner| owner.uid() == self.user)
    }

    /// Gives up root for the login screen's user, for good, and takes over where its desktop is.
    fn become_its_user(&self) -> io::Result<()> {
        let done = |status: libc::c_int| if status == 0 { Ok(()) } else { Err(io::Error::last_os_error()) };
        unsafe {
            let account = libc::getpwuid(self.user);
            if account.is_null() {
                return Err(io::Error::other("the user has no account"));
            }
            done(libc::initgroups((*account).pw_name, self.group))?;
            done(libc::setgid(self.group))?;
            done(libc::setuid(self.user))?;
            // No other thread runs yet, so the environment can be changed.
            std::env::remove_var("XDG_CONFIG_HOME");
            for (name, value) in &self.desktop {
                std::env::set_var(name, value);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_screen_is_known_by_its_class_and_its_display() {
        let greeter = b"HOME=/var/lib/sddm\0XDG_SESSION_CLASS=greeter\0WAYLAND_DISPLAY=wayland-1\0XDG_RUNTIME_DIR=/run/user/961\0";
        let desktop = desktop_of(greeter).expect("a login screen");
        assert_eq!(desktop.len(), 2, "only what leads to the desktop is kept: {desktop:?}");
        assert!(desktop.contains(&("WAYLAND_DISPLAY".into(), "wayland-1".into())));
        assert!(desktop_of(b"XDG_SESSION_CLASS=greeter\0XDG_RUNTIME_DIR=/run/user/961\0").is_none(), "no display");
        assert!(desktop_of(b"XDG_SESSION_CLASS=user\0WAYLAND_DISPLAY=wayland-1\0").is_none(), "a user's session");
        assert!(desktop_of(b"XDG_SESSION_CLASS=greeter\0DISPLAY=:0\0XAUTHORITY=/run/sddm/xauth\0").is_some(), "X11");
    }

    #[test]
    fn only_a_login_screen_s_session_names_its_user() {
        let session = |class: &str, user: u32| {
            format!("# This is private data.\nUID={user}\nUSER=sddm\nACTIVE=1\nCLASS={class}\nLEADER=1203\n")
        };
        assert_eq!(greeter_user(&session("greeter", 961)), Some(961));
        assert_eq!(greeter_user(&session("user", 1000)), None);
        assert_eq!(greeter_user(&session("greeter", 0)), None, "root is never given up for root");
        assert_eq!(greeter_user("CLASS=greeter\n"), None);
    }
}
