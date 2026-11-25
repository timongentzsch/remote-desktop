//! Newer versions: what the releases say there is, whether that is genuine, and putting it in place.
//!
//! A release carries a file `update`: its version on the first line, then the SHA-256 and the name of each of
//! its binaries, as `sha256sum` writes them. `update.sig` is that file's Ed25519 signature by the key whose
//! public half is `KEY`; the private half is a secret of the workflow that publishes releases (see
//! .github/workflows/build.yml). Only a higher version than this one is taken, so that nobody can hand a host
//! an old release with a known fault.
//!
//! An installed host looks once a day and tells its page (see `watch`). Where `install --auto-update` asked
//! for it, the new binary is then put in place while nobody is connected, and the one before comes back if the
//! new one does not serve: on Windows by this program as the user, on Linux as root, which systemd starts
//! when the host writes the version it wants into `wanted_path()` (see platform/linux/install.rs). A Mac asks
//! for a new binary's permissions again, at the Mac, so there `remote-desktop update` is run by hand.

use std::fs;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use ring::{digest, signature};

use crate::install::curl;
use crate::platform::data_directory;
#[cfg(not(target_os = "macos"))]
use crate::platform::install as service;
use crate::server::Host;
use crate::{NAME, fail};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const RELEASES: &str = "https://github.com/timongentzsch/remote-desktop/releases/latest/download";
/// Tests name another place here. What is found there has to be signed all the same.
const RELEASES_ELSEWHERE: &str = "REMOTE_DESKTOP_UPDATE_URL";
const KEY: [u8; 32] = [
    0xa2, 0xb0, 0xdb, 0xe5, 0x7d, 0xa8, 0x7e, 0x7d, 0x92, 0x2f, 0x41, 0x44, 0x44, 0xa5, 0x8b, 0x92, 0x13, 0x63, 0x74,
    0xfa, 0xa0, 0x9e, 0x8f, 0x8f, 0x49, 0xb3, 0x92, 0xfe, 0x02, 0xc5, 0x6c, 0xfb,
];
/// How long a download may take.
const PATIENCE: &str = "120";
/// The host looks once it has settled and the network is likely there, then daily.
const SETTLED: Duration = Duration::from_secs(60);
const DAY: Duration = Duration::from_secs(24 * 3600);
/// How often it looks whether the last viewer has left, and how long a new binary has to start serving.
const IDLE_CHECK: Duration = Duration::from_secs(60);
#[cfg(not(target_os = "macos"))]
const STARTING: Duration = Duration::from_secs(30);

/// Where `install --auto-update` leaves its mark, and the host the version it has asked for.
pub fn wanted_path() -> std::path::PathBuf {
    data_directory().join("update")
}

/// This build's binary among a release's.
fn binary_name() -> String {
    let system = match std::env::consts::OS {
        "macos" => "-macos",
        "windows" => "-windows.exe",
        _ => "",
    };
    format!("{NAME}-{}{system}", std::env::consts::ARCH)
}

fn fetch(file: &str) -> Result<Vec<u8>, String> {
    let from = std::env::var(RELEASES_ELSEWHERE).unwrap_or_else(|_| RELEASES.into());
    let output = curl()
        .args(["--show-error", "--fail", "--location", "--max-time", PATIENCE, &format!("{from}/{file}")])
        .output()
        .map_err(|error| format!("cannot run curl: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(output.stdout)
}

fn genuine(key: &[u8], list: &[u8], signed: &[u8]) -> Result<(), String> {
    signature::UnparsedPublicKey::new(&signature::ED25519, key)
        .verify(list, signed)
        .map_err(|_| "the release's list of binaries is not signed by this program's publisher".into())
}

/// A version as numbers, which compare as versions do.
fn numbers(version: &str) -> Vec<u32> {
    version.split('.').map(|part| part.parse().unwrap_or(0)).collect()
}

/// Whether `list` names `program` as the binary called `name`.
fn listed(list: &str, name: &str, program: &[u8]) -> bool {
    let hash: String =
        digest::digest(&digest::SHA256, program).as_ref().iter().map(|byte| format!("{byte:02x}")).collect();
    list.lines().any(|line| line.split_once("  ") == Some((&hash, name)))
}

/// The newest release's version and list of binaries, if it is newer than this program.
fn newer() -> Result<Option<(String, String)>, String> {
    let (list, signed) = (fetch("update")?, fetch("update.sig")?);
    genuine(&KEY, &list, &signed)?;
    let list = String::from_utf8_lossy(&list).into_owned();
    let version = list.lines().next().unwrap_or_default().to_owned();
    Ok((numbers(&version) > numbers(VERSION)).then_some((version, list)))
}

/// Whether `version` installs itself here: `install --auto-update` left its mark, and the host has not asked
/// for this version before.
fn automatic(version: &str) -> bool {
    fs::read_to_string(wanted_path()).is_ok_and(|wanted| wanted != version)
}

/// Whether this program may put the binary in place itself. On Windows it is in the user's own folder; on
/// Linux root may, which is who systemd runs this as. Otherwise the new binary's `install` does, through sudo.
#[cfg(windows)]
fn in_place() -> bool {
    true
}
#[cfg(target_os = "linux")]
fn in_place() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// What an installed host does besides serving: looks for a newer version, which its page then says there is
/// (`host.newer`: the version and whether it installs itself), and asks for it once nobody is connected. Once
/// only for each version, so that one that did not serve here, and went, is not put in place day after day.
pub fn watch(host: Arc<Host>) {
    thread::sleep(SETTLED);
    loop {
        match newer() {
            Ok(Some((version, _))) => {
                let automatic = automatic(&version);
                *host.newer.lock().unwrap() = Some((version.clone(), automatic));
                if automatic {
                    while Arc::strong_count(&host.viewers) > 1 {
                        thread::sleep(IDLE_CHECK);
                    }
                    match fs::write(wanted_path(), &version) {
                        // On Linux, systemd sees the file change.
                        #[cfg(windows)]
                        Ok(()) => service::ask_for_update(),
                        #[cfg(not(windows))]
                        Ok(()) => {}
                        Err(error) => warn!("cannot ask for version {version}: {error}"),
                    }
                }
            }
            Ok(None) => {}
            Err(error) => warn!("could not look for a newer version: {error}"),
        }
        thread::sleep(DAY);
    }
}

/// `remote-desktop update`: installs the newest release if it is newer than this program.
pub fn update() {
    let Some((version, list)) = newer().unwrap_or_else(|error| fail(&error)) else {
        println!("Version {VERSION} is the newest.");
        return;
    };
    let name = binary_name();
    let program = fetch(&name).unwrap_or_else(|error| fail(&error));
    if !listed(&list, &name, &program) {
        fail(&format!("what was downloaded as {name} is not version {version}'s binary"));
    }
    #[cfg(not(target_os = "macos"))]
    if in_place() {
        return replace(&program, &version);
    }
    install(&program)
}

/// Lets the new binary install itself, as when it was downloaded by hand: it asks for what that takes.
fn install(program: &[u8]) {
    let directory = std::env::temp_dir().join(format!("{NAME}-update-{}", std::process::id()));
    let new = directory.join(NAME);
    let status = fs::create_dir(&directory)
        .and_then(|()| fs::write(&new, program))
        .and_then(|()| executable(&new))
        .and_then(|()| Command::new(&new).arg("install").status());
    let _ = fs::remove_dir_all(&directory);
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) => fail(&format!("cannot run the new version: {error}")),
    }
}

fn executable(_program: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    fs::set_permissions(_program, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    Ok(())
}

/// Puts the new binary where the installed one is and starts it in its place. If the host served before and
/// does not within `STARTING` afterwards, the binary from before is put back and started.
#[cfg(not(target_os = "macos"))]
fn replace(program: &[u8], version: &str) {
    let binary = service::binary();
    let old = binary.with_extension("old");
    let serves = || crate::install::serves(crate::port());
    let served = serves();
    service::stop_for_update();
    let put = fs::rename(&binary, &old).and_then(|()| fs::write(&binary, program)).and_then(|()| executable(&binary));
    service::start_updated();
    let began = std::time::Instant::now();
    while put.is_ok() && served && !serves() && began.elapsed() < STARTING {
        thread::sleep(Duration::from_millis(500));
    }
    if put.is_ok() && (!served || serves()) {
        let _ = fs::remove_file(&old); // Windows keeps it for as long as this program, which it is, runs
        println!("Version {version} is installed and running.");
        return;
    }
    service::stop_for_update();
    let back = fs::rename(&old, &binary);
    service::start_updated();
    match (put, back) {
        (Err(error), _) => fail(&format!("cannot write {}: {error}", binary.display())),
        (_, Err(error)) => fail(&format!("version {version} does not serve here, and {VERSION} is not back: {error}")),
        _ => fail(&format!("version {version} did not serve here; {VERSION} is back in its place")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;

    #[test]
    fn only_a_signed_list_is_genuine_and_only_a_higher_version_newer() {
        let random = ring::rand::SystemRandom::new();
        let pair = signature::Ed25519KeyPair::generate_pkcs8(&random).unwrap();
        let pair = signature::Ed25519KeyPair::from_pkcs8(pair.as_ref()).unwrap();
        let key = pair.public_key().as_ref();
        let list = b"0.10.3\nabc  remote-desktop-x86_64\n";
        let signed = pair.sign(list);
        assert!(genuine(key, list, signed.as_ref()).is_ok());
        assert!(genuine(key, b"9.0.0\nabc  remote-desktop-x86_64\n", signed.as_ref()).is_err());
        assert!(genuine(&KEY, list, signed.as_ref()).is_err());

        assert!(numbers("0.10.3") > numbers("0.10.2") && numbers("0.10.2") > numbers("0.9.12"));
        assert!(numbers("1.0.0") > numbers("0.99.99") && numbers("0.10.2") == numbers("0.10.2"));
        assert!(numbers("") < numbers("0.0.1") && numbers("newest") < numbers("0.0.1"));
    }

    #[test]
    fn a_binary_is_listed_by_its_hash_under_its_own_name() {
        // `printf abc | sha256sum`
        let hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let list = format!("0.10.3\n{hash}  remote-desktop-x86_64\n{hash}0  remote-desktop-aarch64\n");
        assert!(listed(&list, "remote-desktop-x86_64", b"abc"));
        assert!(!listed(&list, "remote-desktop-x86_64", b"abd"));
        assert!(!listed(&list, "remote-desktop-aarch64", b"abc"));
    }
}
