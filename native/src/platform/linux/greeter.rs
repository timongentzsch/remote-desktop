//! The login screen. A display manager shows it in a desktop session of its own, run by a user of its own,
//! before anyone has logged in. The host gets there by starting as root, waiting for such a session, and then
//! becoming that user in that session's environment: from then on it is the host as in any desktop session.
//! When the login screen goes, this process goes too, and the system service that runs it starts over.
//!
//! A login screen can also come while a user is logged in: where it is the lock screen, as with LightDM, and
//! where one user makes room for another. The seat then shows it and no longer the user's session, so the
//! host of that session stands back for as long (see `stand_back`), and this one has the port. Which session
//! a seat shows is logind's to say.

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::{fail, server};

/// How often the host at the login screen looks whether that screen is still there.
const STILL_THERE: Duration = Duration::from_secs(1);
/// Where logind keeps a file for each session. A change there is what wakes the search for a login screen;
/// nothing is read from it.
const SESSIONS: &str = "/run/systemd/sessions";
/// What a display manager puts into the environment of its login screen's processes.
const GREETER: (&str, &str) = ("XDG_SESSION_CLASS", "greeter");
/// The variables by which a process finds the desktop session it runs in, a display among them.
const DISPLAYS: [&str; 2] = ["WAYLAND_DISPLAY", "DISPLAY"];
const DESKTOP: [&str; 5] = ["XDG_RUNTIME_DIR", "WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY", "DBUS_SESSION_BUS_ADDRESS"];
/// What a user's host that starts over tells itself: that it had a seat (see `stand_back`).
const SEATED: &str = "REMOTE_DESKTOP_SEATED";
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
            thread::sleep(STILL_THERE);
        }
        std::process::exit(0) // someone logged in, or the display manager stopped
    });
    server::serve(listener, host)
}

/// A session as logind describes it in its directory: whose it is, whether it is a login screen's, whether it
/// is the one its seat shows, and that seat, which a session from elsewhere, as over ssh, has none of.
#[derive(Debug, PartialEq)]
struct Session {
    user: u32,
    greeter: bool,
    shown: bool,
    seat: String,
}

fn session(told: &str) -> Option<Session> {
    let value = |name: &str| told.lines().find_map(|line| line.strip_prefix(name)?.strip_prefix('='));
    Some(Session {
        user: value("UID")?.parse().ok()?,
        greeter: value("CLASS")? == GREETER.1,
        shown: value("ACTIVE") == Some("1"),
        seat: value("SEAT").unwrap_or_default().to_owned(),
    })
}

fn sessions() -> Vec<Session> {
    let files = fs::read_dir(SESSIONS).into_iter().flatten().flatten();
    // Older versions of logind keep a pipe beside each session's file, which nothing is to be read from.
    let described = files.filter(|file| file.file_type().is_ok_and(|kind| kind.is_file()));
    described.filter_map(|file| session(&fs::read_to_string(file.path()).ok()?)).collect()
}

/// The users of the login screens that their seats show. Only logind's word counts for whose processes are
/// looked at: what a process has in its environment is its owner's to say, and this service hands whoever it
/// takes for the login screen the port and the password. Root is never given up for root.
fn greeters() -> Vec<u32> {
    let shown = sessions().into_iter().filter(|session| session.greeter && session.shown && session.user != 0);
    shown.map(|session| session.user).collect()
}

/// Whether `user` has a session on a seat, and whether such a seat shows a login screen.
fn seat_of(user: u32, sessions: &[Session]) -> (bool, bool) {
    let own = sessions.iter().filter(|session| session.user == user && !session.seat.is_empty());
    let seats: Vec<&str> = own.map(|session| &*session.seat).collect();
    let login = |session: &Session| session.greeter && session.shown && seats.contains(&&*session.seat);
    (!seats.is_empty(), sessions.iter().any(login))
}

/// What the host of a user's session does about login screens: it waits while its seat shows one, and from
/// then on leaves as soon as it does, or when the user has no session on a seat any more. A host that never
/// had one, as on a machine without a seat, is none of this.
///
/// It leaves by starting over, which lets go of the port and of every client at once, and brings it back here
/// to wait. When the user's session has ended, it ends: the next login starts it.
pub fn stand_back() {
    let user = unsafe { libc::getuid() };
    let look = move || seat_of(user, &sessions());
    let ended = || std::process::exit(0);
    // A host that started over knows that it had a seat, which one that waits here may lose meanwhile.
    let mut seated = std::env::var_os(SEATED).is_some();
    loop {
        match look() {
            (true, true) => seated = true,
            (true, false) => break,
            (false, _) if seated => ended(),
            (false, _) => return,
        }
        thread::sleep(LOOK);
    }
    thread::spawn(move || {
        loop {
            thread::sleep(STILL_THERE);
            match look() {
                (true, false) => {}
                (true, true) => {
                    let again = std::env::current_exe()
                        .map(|this| Command::new(this).args(std::env::args_os().skip(1)).env(SEATED, "1").exec());
                    fail(&format!("cannot start over: {}", again.unwrap_or_else(|error| error)));
                }
                (false, _) => ended(),
            }
        }
    });
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
        if crate::platform::unix::fd::wait_readable([watch], patience) == [true] {
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
    let users = greeters();
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
    /// Whether the process this was found by is still the login screen's, and its seat still shows that.
    fn is_there(&self) -> bool {
        fs::metadata(format!("/proc/{}", self.process)).is_ok_and(|owner| owner.uid() == self.user)
            && greeters().contains(&self.user)
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
    fn a_session_is_read_as_logind_describes_it() {
        let told = |class: &str, user: u32, shown: u8, seat: &str| {
            format!(
                "# This is private data.\nUID={user}\nUSER=sddm\nACTIVE={shown}\n{seat}CLASS={class}\nLEADER=1203\n"
            )
        };
        let greeter = session(&told("greeter", 961, 1, "SEAT=seat0\n")).unwrap();
        assert_eq!(greeter, Session { user: 961, greeter: true, shown: true, seat: "seat0".into() });
        let behind = session(&told("user", 1000, 0, "SEAT=seat0\n")).unwrap();
        assert!(!behind.greeter && !behind.shown && behind.seat == "seat0", "a user's session behind a login screen");
        assert_eq!(session(&told("user", 1000, 1, "")).unwrap().seat, "", "over ssh: no seat");
        assert_eq!(session("CLASS=greeter\n"), None);
    }

    #[test]
    fn a_user_s_host_stands_back_for_a_login_screen_that_its_seat_shows() {
        let session = |user, greeter, shown, seat: &str| Session { user, greeter, shown, seat: seat.into() };
        let (own, behind) = (session(1000, false, true, "seat0"), session(1000, false, false, "seat0"));
        let ssh = session(1000, false, true, "");
        assert_eq!(seat_of(1000, &[own, ssh]), (true, false), "the seat shows the user's session");
        let lock = [behind, session(112, true, true, "seat0")];
        assert_eq!(seat_of(1000, &lock), (true, true), "a login screen in front, as a lock screen is");
        let waiting = [session(1000, false, true, "seat0"), session(112, true, false, "seat0")];
        assert_eq!(seat_of(1000, &waiting), (true, false), "one that waits behind the session");
        let elsewhere = [session(1000, false, true, "seat0"), session(112, true, true, "seat1")];
        assert_eq!(seat_of(1000, &elsewhere), (true, false), "another seat's");
        let gone = [session(1000, false, true, ""), session(112, true, true, "seat0")];
        assert_eq!(seat_of(1000, &gone), (false, false), "no session on a seat: logged out, or never had one");
    }
}
