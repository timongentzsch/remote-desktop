//! Whose desktop a host serves, which is logind's to say: it knows the sessions, their seats, and which session
//! a seat shows.
//!
//! The login screen: a display manager shows it in a desktop session of its own, run by a user of its own,
//! before anyone has logged in. The host gets there by starting as root, waiting for such a session, and then
//! becoming that user in that session's environment: from then on it is the host as in any desktop session.
//! When the login screen goes, this process goes too, and the system service that runs it starts over.
//!
//! A user's session: the user's host starts with the user's services, which may be long before the desktop,
//! and waits for a session of the user on a seat whose display it can reach (see `await_desktop`). It starts
//! over, and so waits again, when that session ends, and when a login screen comes while the user is logged
//! in: where that is the lock screen, as with LightDM, and where one user makes room for another. The seat
//! then shows the login screen, and the host at the login screen has the port.

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::fd::RawFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use super::{compositor, input};
use crate::platform::unix::fd::wait_readable;
use crate::{fail, server};

/// How often a host looks whether the desktop it serves is still the one to serve.
const STILL_THERE: Duration = Duration::from_secs(1);
/// Where logind keeps a file for each session, named as the session is. A change there is what wakes the
/// search for a desktop.
const SESSIONS: &str = "/run/systemd/sessions";
/// Where X servers have their sockets, each named X and the display's number.
const X_SOCKETS: &str = "/tmp/.X11-unix";
/// What a display manager puts into the environment of its login screen's processes, and logind into that of
/// every session's: the session's name.
const GREETER: (&str, &str) = ("XDG_SESSION_CLASS", "greeter");
const SESSION_NAME: &str = "XDG_SESSION_ID";
/// The variables by which a process finds the desktop session it runs in, a display among them: those that
/// are the user's whatever the session, and those of one session.
const DISPLAYS: [&str; 2] = ["WAYLAND_DISPLAY", "DISPLAY"];
const OF_THE_USER: [&str; 2] = ["XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"];
const OF_THE_SESSION: [&str; 4] = ["WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY", "HYPRLAND_INSTANCE_SIGNATURE"];
/// What the host at the login screen takes over of them.
const DESKTOP: [&str; 5] = ["XDG_RUNTIME_DIR", "WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY", "DBUS_SESSION_BUS_ADDRESS"];
/// How often a desktop is looked for while its session is coming up, and how many times after a change.
const LOOK: Duration = Duration::from_millis(500);
const LOOKS: u32 = 30;

/// Where a desktop is: the variables that lead to it.
type Desktop = Vec<(String, String)>;

/// A login screen: one of its processes, the user it runs as, and where its desktop is.
struct Greeter {
    process: u32,
    user: u32,
    group: u32,
    desktop: Desktop,
}

/// Serves the login screen whenever there is one. Has to be started as root.
pub fn run(address: &str) -> ! {
    let mut watch = Watch::new(&[Path::new(SESSIONS)]);
    let greeter = loop {
        match find() {
            Some(greeter) => break greeter,
            None => watch.pause(),
        }
    };
    drop(watch);
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

/// A session as logind describes it in its directory: its name, whose it is, whether it is a login screen's,
/// whether it is the one its seat shows, and that seat, which a session from elsewhere, as over ssh, has none
/// of.
#[derive(Debug, Default, PartialEq)]
struct Session {
    name: String,
    user: u32,
    greeter: bool,
    shown: bool,
    seat: String,
}

fn session(name: &str, told: &str) -> Option<Session> {
    let value = |name: &str| told.lines().find_map(|line| line.strip_prefix(name)?.strip_prefix('='));
    Some(Session {
        name: name.to_owned(),
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
    let read = |file: fs::DirEntry| session(file.file_name().to_str()?, &fs::read_to_string(file.path()).ok()?);
    described.filter_map(read).collect()
}

/// The users of the login screens that their seats show. Only logind's word counts for whose processes are
/// looked at: what a process has in its environment is its owner's to say, and this service hands whoever it
/// takes for the login screen the port and the password. Root is never given up for root.
fn greeters() -> Vec<u32> {
    let shown = sessions().into_iter().filter(|session| session.greeter && session.shown && session.user != 0);
    shown.map(|session| session.user).collect()
}

/// The sessions `user` has on a seat, and whether such a seat shows a login screen.
fn seat_of(user: u32, sessions: &[Session]) -> (Vec<&Session>, bool) {
    let own = || sessions.iter().filter(|session| session.user == user && !session.seat.is_empty());
    let login = |session: &Session| session.greeter && session.shown && own().any(|own| own.seat == session.seat);
    (own().collect(), sessions.iter().any(login))
}

/// What the host of a user's session does before it serves: it finds the desktop. That is a session of the
/// user which its seat shows, and no login screen instead, with a display that can be reached: the
/// one this process was told of when it started, the one the desktop told the user's systemd of, or the one
/// a process of that session knows. Until there is one it waits here, as a host does that started with the
/// user's services at boot, before anyone logged in.
///
/// From then on it looks whether that still holds, and starts over when it does not: the session ended, its
/// display went, or a login screen came. Starting over lets go of the port and of every client at once, and
/// brings it back here to wait.
///
/// Where the user has no session on a seat, a host that was told of a display it can reach serves that, as one
/// run by hand for a desktop without a seat. Without logind the desktop is what the host was told of.
pub fn await_desktop() {
    if !Path::new(SESSIONS).is_dir() {
        return;
    }
    let user = unsafe { libc::getuid() };
    // What this process was told of a desktop when it started, to which it goes back from one that fails.
    let leading = [&OF_THE_USER[..], &OF_THE_SESSION[..]].concat().into_iter();
    let before: Desktop = leading.filter_map(|name| Some((name.to_owned(), std::env::var(name).ok()?))).collect();
    let told = || usable(before.clone());
    // Whether the desktop lets this process in: an X server wants the session's key, which is written a
    // moment after the server is up. No other thread runs yet, so the environment can be changed.
    let taken = |desktop: &Desktop| {
        take_over(desktop);
        let admitted = compositor::available() || x11rb::connect(None).is_ok();
        if !admitted {
            take_over(&before);
        }
        admitted
    };
    let runtime = std::env::var_os(OF_THE_USER[0]).map_or_else(|| format!("/run/user/{user}").into(), PathBuf::from);
    let mut watch = Watch::new(&[Path::new(SESSIONS), &runtime, Path::new(X_SOCKETS)]);
    let seated = loop {
        let sessions = sessions();
        let found = match seat_of(user, &sessions) {
            (own, false) if own.is_empty() => told().is_some_and(|desktop| taken(&desktop)).then_some(false),
            // The session has to be the one its seat shows: only then may its user create input devices.
            (own, false) if own.iter().any(|session| session.shown) => {
                let names: Vec<&str> = own.iter().map(|session| &*session.name).collect();
                let known = || known_to(user, &names);
                let sources: [&dyn Fn() -> Option<Desktop>; 3] = [&told, &told_to_systemd, &known];
                sources.iter().filter_map(|source| source()).any(|desktop| taken(&desktop)).then_some(true)
            }
            _ => None,
        };
        match found {
            Some(seated) => break seated,
            None => watch.pause(),
        }
    };
    drop(watch);
    if !seated {
        return;
    }
    // logind lets the user of the session that a seat shows create input devices, a moment after it shows it.
    let device = CString::new(input::DEVICE).unwrap_or_default();
    for _ in 0..LOOKS {
        if unsafe { libc::access(device.as_ptr(), libc::W_OK) } == 0 {
            break;
        }
        thread::sleep(LOOK);
    }
    let desktop: Desktop = OF_THE_USER
        .iter()
        .chain(&OF_THE_SESSION)
        .filter_map(|name| Some((name.to_string(), std::env::var(name).ok()?)))
        .collect();
    thread::spawn(move || {
        loop {
            thread::sleep(STILL_THERE);
            let sessions = sessions();
            if !matches!(seat_of(user, &sessions), (own, false) if !own.is_empty()) || !display_is_there(&desktop) {
                start_over();
            }
        }
    });
}

/// Makes `desktop` the one this process is in: what it knew of a session goes, and what leads to this one is
/// set.
fn take_over(desktop: &Desktop) {
    for name in OF_THE_SESSION {
        unsafe { std::env::remove_var(name) };
    }
    for (name, value) in desktop {
        unsafe { std::env::set_var(name, value) };
    }
}

/// Runs this program anew in this process's place, without what it knew of a session.
fn start_over() -> ! {
    let again = std::env::current_exe().map(|this| {
        let mut again = Command::new(this);
        again.args(std::env::args_os().skip(1));
        for name in OF_THE_SESSION {
            again.env_remove(name);
        }
        again.exec()
    });
    fail(&format!("cannot start over: {}", again.unwrap_or_else(|error| error)))
}

/// What the desktop told the user's systemd of itself, which is how the user's services find it.
fn told_to_systemd() -> Option<Desktop> {
    let told = Command::new("systemctl").args(["--user", "show-environment"]).output().ok()?;
    let told = String::from_utf8_lossy(&told.stdout);
    let leads = |name: &str| OF_THE_USER.contains(&name) || OF_THE_SESSION.contains(&name);
    let leading = told.lines().filter_map(|line| line.split_once('=')).filter(|(name, _)| leads(name));
    usable(leading.map(|(name, value)| (name.to_owned(), value.to_owned())).collect())
}

/// What a process of one of the `sessions` of `user` knows of its desktop.
fn known_to(user: u32, sessions: &[&str]) -> Option<Desktop> {
    let of_session = |name: &str, value: &str| name == SESSION_NAME && sessions.contains(&value);
    fs::read_dir("/proc").ok()?.flatten().find_map(|entry| {
        let own = entry.metadata().is_ok_and(|owner| owner.uid() == user);
        let environment = fs::read(entry.path().join("environ")).ok().filter(|_| own)?;
        usable(desktop_of(&environment, of_session, &[&OF_THE_USER[..], &OF_THE_SESSION[..]].concat())?)
    })
}

/// The path of a display's socket: a Wayland compositor's, by its name in the user's runtime directory or by
/// its whole path, or a local X server's, by its number. None for a display elsewhere.
fn socket_of(display: &str, value: &str, desktop: &Desktop) -> Option<PathBuf> {
    let runtime = desktop.iter().find(|(name, _)| name == OF_THE_USER[0]).map(|(_, value)| value.as_str());
    match display {
        "WAYLAND_DISPLAY" => Some(Path::new(runtime.unwrap_or_default()).join(value)),
        _ => {
            let number = value.strip_prefix(':')?.split('.').next()?;
            Some(Path::new(X_SOCKETS).join(format!("X{number}")))
        }
    }
}

/// `desktop` with the displays that can be reached: something answers at their sockets. A display elsewhere
/// is taken at its word. None if no display is left.
fn usable(mut desktop: Desktop) -> Option<Desktop> {
    desktop.retain(|(_, value)| !value.is_empty());
    let all = desktop.clone();
    desktop.retain(|(name, value)| {
        !DISPLAYS.contains(&&**name)
            || socket_of(name, value, &all).is_none_or(|socket| UnixStream::connect(socket).is_ok())
    });
    desktop.iter().any(|(name, _)| DISPLAYS.contains(&&**name)).then_some(desktop)
}

/// Whether the sockets of the displays of `desktop` are still there, which is cheaper to ask every second
/// than whether something answers.
fn display_is_there(desktop: &Desktop) -> bool {
    let displays = desktop.iter().filter(|(name, _)| DISPLAYS.contains(&&**name));
    displays.into_iter().all(|(name, value)| socket_of(name, value, desktop).is_none_or(|socket| socket.exists()))
}

/// Changes in directories, which wake a search for a desktop: a session of logind's, a display's socket.
struct Watch {
    changes: RawFd,
    watching: bool,
    looks_left: u32,
}

impl Watch {
    fn new(directories: &[&Path]) -> Self {
        let changes = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        let kinds = libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_TO | libc::IN_CLOSE_WRITE;
        let watch = |directory: &&Path| {
            let path = CString::new(directory.as_os_str().as_encoded_bytes());
            path.is_ok_and(|path| unsafe { libc::inotify_add_watch(changes, path.as_ptr(), kinds) } >= 0)
        };
        // The first is the one that has to be watched: where it cannot be, the search goes on for good.
        let watching = directories.iter().map(watch).collect::<Vec<_>>().first() == Some(&true);
        Self { changes, watching, looks_left: LOOKS }
    }

    /// Waits for a change. A session shows up before its processes have their display, and a display before
    /// the desktop has told of it, so after a change the wait ends some times over without one.
    fn pause(&mut self) {
        let patience = (self.looks_left > 0 || !self.watching).then_some(LOOK);
        if wait_readable([self.changes], patience) == [true] {
            let mut events = [0u8; 4096];
            unsafe { libc::read(self.changes, events.as_mut_ptr().cast(), events.len()) };
            self.looks_left = LOOKS;
        } else {
            self.looks_left = self.looks_left.saturating_sub(1);
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        unsafe { libc::close(self.changes) };
    }
}

/// The login screen, if a process says it belongs to one and knows a display. A Wayland one is preferred: a
/// login screen on Wayland has an X server as well, for old programs.
fn find() -> Option<Greeter> {
    let users = greeters();
    let of_greeter = |name: &str, value: &str| (name, value) == GREETER;
    let mut found: Vec<Greeter> = fs::read_dir("/proc")
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let process = entry.file_name().to_str()?.parse().ok()?;
            let owner = entry.metadata().ok()?;
            let environment = fs::read(entry.path().join("environ")).ok().filter(|_| users.contains(&owner.uid()))?;
            let desktop = desktop_of(&environment, of_greeter, &DESKTOP)?;
            Some(Greeter { process, user: owner.uid(), group: owner.gid(), desktop })
        })
        .collect();
    found.sort_by_key(|greeter| !greeter.desktop.iter().any(|(name, _)| name == DISPLAYS[0]));
    found.into_iter().next()
}

/// Where a process's desktop is, from its environment as the kernel shows it: what it has of `kept`, if one
/// of its variables `belongs` to the session that is looked for and it knows a display.
fn desktop_of(environment: &[u8], belongs: impl Fn(&str, &str) -> bool, kept: &[&str]) -> Option<Desktop> {
    let variables: Vec<(&str, &str)> = environment
        .split(|&byte| byte == 0)
        .filter_map(|variable| std::str::from_utf8(variable).ok()?.split_once('='))
        .collect();
    let has = |wanted: &str| variables.iter().any(|(name, value)| *name == wanted && !value.is_empty());
    if !variables.iter().any(|(name, value)| belongs(name, value)) || !DISPLAYS.iter().any(|display| has(display)) {
        return None;
    }
    let kept = variables.iter().filter(|(name, value)| kept.contains(name) && !value.is_empty());
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
        let desktop_of = |environment: &[u8]| desktop_of(environment, |name, value| (name, value) == GREETER, &DESKTOP);
        let greeter = b"HOME=/var/lib/sddm\0XDG_SESSION_CLASS=greeter\0WAYLAND_DISPLAY=wayland-1\0XDG_RUNTIME_DIR=/run/user/961\0";
        let desktop = desktop_of(greeter).expect("a login screen");
        assert_eq!(desktop.len(), 2, "only what leads to the desktop is kept: {desktop:?}");
        assert!(desktop.contains(&("WAYLAND_DISPLAY".into(), "wayland-1".into())));
        assert!(desktop_of(b"XDG_SESSION_CLASS=greeter\0XDG_RUNTIME_DIR=/run/user/961\0").is_none(), "no display");
        assert!(desktop_of(b"XDG_SESSION_CLASS=user\0WAYLAND_DISPLAY=wayland-1\0").is_none(), "a user's session");
        assert!(desktop_of(b"XDG_SESSION_CLASS=greeter\0DISPLAY=:0\0XAUTHORITY=/run/sddm/xauth\0").is_some(), "X11");
    }

    #[test]
    fn a_user_s_desktop_is_known_to_the_processes_of_the_session() {
        let kept = [&OF_THE_USER[..], &OF_THE_SESSION[..]].concat();
        let desktop_of =
            |environment: &[u8]| desktop_of(environment, |name, value| (name, value) == (SESSION_NAME, "3"), &kept);
        let terminal =
            b"XDG_SESSION_ID=3\0WAYLAND_DISPLAY=wayland-1\0HYPRLAND_INSTANCE_SIGNATURE=abc\0HOME=/home/a\0TERM=\0";
        let desktop = desktop_of(terminal).expect("a process of the session");
        assert_eq!(desktop.len(), 2, "what leads to the desktop: {desktop:?}");
        assert!(desktop_of(b"XDG_SESSION_ID=7\0DISPLAY=localhost:10.0\0").is_none(), "another session's, as over ssh");
        assert!(desktop_of(b"XDG_SESSION_ID=3\0HOME=/home/a\0").is_none(), "the compositor itself: no display");
    }

    #[test]
    fn a_display_is_found_by_its_socket() {
        let desktop: Desktop = vec![("XDG_RUNTIME_DIR".into(), "/run/user/1000".into())];
        let socket = |display, value| socket_of(display, value, &desktop);
        assert_eq!(socket("WAYLAND_DISPLAY", "wayland-1"), Some("/run/user/1000/wayland-1".into()));
        assert_eq!(socket("WAYLAND_DISPLAY", "/tmp/own"), Some("/tmp/own".into()), "a whole path");
        assert_eq!(socket("DISPLAY", ":1"), Some("/tmp/.X11-unix/X1".into()));
        assert_eq!(socket("DISPLAY", ":0.0"), Some("/tmp/.X11-unix/X0".into()));
        assert_eq!(socket("DISPLAY", "localhost:10.0"), None, "elsewhere");
        let gone = vec![("WAYLAND_DISPLAY".to_owned(), "/nowhere/wayland-9".to_owned())];
        assert_eq!(usable(gone.clone()), None, "nothing answers");
        let other = [gone, vec![("DISPLAY".to_owned(), "elsewhere:0".to_owned())]].concat();
        assert_eq!(usable(other).map(|desktop| desktop.len()), Some(1), "the display that is left");
    }

    #[test]
    fn a_session_is_read_as_logind_describes_it() {
        let told = |class: &str, user: u32, shown: u8, seat: &str| {
            format!(
                "# This is private data.\nUID={user}\nUSER=sddm\nACTIVE={shown}\n{seat}CLASS={class}\nLEADER=1203\n"
            )
        };
        let greeter = session("c1", &told("greeter", 961, 1, "SEAT=seat0\n")).unwrap();
        let expected = Session { name: "c1".into(), user: 961, greeter: true, shown: true, seat: "seat0".into() };
        assert_eq!(greeter, expected);
        let behind = session("2", &told("user", 1000, 0, "SEAT=seat0\n")).unwrap();
        assert!(!behind.greeter && !behind.shown && behind.seat == "seat0", "a user's session behind a login screen");
        assert_eq!(session("5", &told("user", 1000, 1, "")).unwrap().seat, "", "over ssh: no seat");
        assert_eq!(session("1", "CLASS=greeter\n"), None);
    }

    #[test]
    fn a_user_s_host_serves_a_session_on_a_seat_that_shows_no_login_screen() {
        let session = |name: &str, user, greeter, shown, seat: &str| Session {
            name: name.into(),
            user,
            greeter,
            shown,
            seat: seat.into(),
        };
        let seat_of = |sessions: &[Session]| {
            let (own, login) = seat_of(1000, sessions);
            (own.iter().map(|session| session.name.clone()).collect::<Vec<_>>().join(" "), login)
        };
        let (own, behind) = (session("2", 1000, false, true, "seat0"), session("2", 1000, false, false, "seat0"));
        let ssh = session("7", 1000, false, true, "");
        assert_eq!(seat_of(&[own, ssh]), ("2".into(), false), "the seat shows the user's session");
        let lock = [behind, session("c2", 112, true, true, "seat0")];
        assert_eq!(seat_of(&lock), ("2".into(), true), "a login screen in front, as a lock screen is");
        let waiting = [session("2", 1000, false, true, "seat0"), session("c2", 112, true, false, "seat0")];
        assert_eq!(seat_of(&waiting), ("2".into(), false), "one that waits behind the session");
        let elsewhere = [session("2", 1000, false, true, "seat0"), session("c2", 112, true, true, "seat1")];
        assert_eq!(seat_of(&elsewhere), ("2".into(), false), "another seat's");
        let gone = [session("7", 1000, false, true, ""), session("c2", 112, true, true, "seat0")];
        assert_eq!(seat_of(&gone), (String::new(), false), "no session on a seat: logged out, or never had one");
    }
}
