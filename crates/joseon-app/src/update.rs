//! Updates: look for a newer release, fetch it, put it in the place of this program, start it.
//!
//! The release list is the public one of the project on GitHub. The program asks for it once after the
//! start and once a day (a setting), and when the user says so. Nothing is sent but the request itself.
//! The download and the unpacking are done by `curl` and `tar`, which Windows 10 and later bring along:
//! the program needs no network code of its own.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// The release list. `JOSEON_UPDATE_URL` puts another one in its place (for a test).
pub const RELEASES_URL: &str = "https://api.github.com/repos/jasas1/JoseonPC/releases?per_page=10";
/// A file of a release may only come from these places.
const DOWNLOAD_PREFIXES: [&str; 2] = ["https://github.com/jasas1/", "https://objects.githubusercontent.com/"];
/// The end of the name of the file for this system.
pub const ASSET_SUFFIX: &str = "-windows-x64.zip";
/// The argument the new program gets: the old one may still run for a moment.
pub const AFTER_UPDATE: &str = "--after-update";

/// A version as the tags write it: "v1.2.3".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// "v0.0.7", "0.0.7". `None` for everything else (a tag such as "nightly" is no version).
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().trim_start_matches('v').split('.');
        let mut next = || parts.next()?.parse::<u32>().ok();
        let version = Version(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }

    /// The version of this program.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Version(0, 0, 0))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// A release that has a file for this system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub asset_name: String,
    pub asset_url: String,
    /// The SHA-256 of the file as GitHub tells it, in hex. `None` when the list has none.
    pub sha256: Option<String>,
}

/// The newest release of the list that has a file for this system. Drafts do not count.
pub fn newest_release(json: &str) -> Result<Option<Release>, String> {
    let list: Value = serde_json::from_str(json).map_err(|_| "the release list is not readable".to_string())?;
    let Some(list) = list.as_array() else {
        // GitHub answers with an object and a message when it refuses (a limit, a missing project).
        let message = list["message"].as_str().unwrap_or("the release list is not a list");
        return Err(message.to_string());
    };
    let mut best: Option<Release> = None;
    for release in list {
        if release["draft"].as_bool().unwrap_or(false) {
            continue;
        }
        let Some(version) = release["tag_name"].as_str().and_then(Version::parse) else {
            continue;
        };
        let asset = release["assets"].as_array().and_then(|assets| {
            assets
                .iter()
                .find(|a| a["name"].as_str().is_some_and(|name| name.ends_with(ASSET_SUFFIX)))
        });
        let Some(asset) = asset else { continue };
        let (Some(name), Some(url)) = (asset["name"].as_str(), asset["browser_download_url"].as_str()) else {
            continue;
        };
        if !DOWNLOAD_PREFIXES.iter().any(|prefix| url.starts_with(prefix)) {
            continue;
        }
        if best.as_ref().is_none_or(|b| version > b.version) {
            best = Some(Release {
                version,
                asset_name: name.to_string(),
                asset_url: url.to_string(),
                sha256: asset["digest"]
                    .as_str()
                    .and_then(|d| d.strip_prefix("sha256:"))
                    .map(str::to_lowercase),
            });
        }
    }
    Ok(best)
}

/// Where the updater stands. The menu shows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    /// Nothing was asked yet.
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Version),
    /// The new program is in place: start it and end this one.
    Ready(Version),
    Failed(String),
}

impl Status {
    /// The line of the menu that says where the updater stands. `None` when there is nothing to say.
    pub fn line(&self) -> Option<String> {
        Some(match self {
            Status::Idle => return None,
            Status::Checking => "Looking for updates\u{2026}".to_string(),
            Status::UpToDate => format!("Joseon {} is the newest version", Version::current()),
            Status::Available(release) => format!("Joseon {} is there", release.version),
            Status::Downloading(version) => format!("Fetching Joseon {version}\u{2026}"),
            Status::Ready(version) => format!("Joseon {version} starts\u{2026}"),
            Status::Failed(why) => format!("Update failed: {why}"),
        })
    }

    pub fn is_busy(&self) -> bool {
        matches!(self, Status::Checking | Status::Downloading(_) | Status::Ready(_))
    }
}

/// `curl` or `tar` with no console window.
fn tool(name: &str) -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut command = Command::new(name);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
}

fn fetch_text(url: &str) -> Result<String, String> {
    let output = tool("curl")
        .args(["-sSL", "--max-time", "20", "-H", "Accept: application/vnd.github+json"])
        .args(["-A", &format!("Joseon/{}", Version::current()), url])
        .output()
        .map_err(|_| "curl is not there (Windows 10 or later has it)".to_string())?;
    if !output.status.success() {
        return Err("no answer from the update server".to_string());
    }
    String::from_utf8(output.stdout).map_err(|_| "the release list is not text".to_string())
}

fn fetch_file(url: &str, to: &Path) -> Result<(), String> {
    let status = tool("curl")
        .args([
            "-sSLf",
            "--max-time",
            "300",
            "-A",
            &format!("Joseon/{}", Version::current()),
        ])
        .arg("-o")
        .arg(to)
        .arg(url)
        .status()
        .map_err(|_| "curl is not there".to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| "the download did not finish".to_string())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Asks the release list for a release newer than `current`.
pub fn check(current: Version) -> Result<Option<Release>, String> {
    let url = std::env::var("JOSEON_UPDATE_URL").unwrap_or_else(|_| RELEASES_URL.to_string());
    Ok(newest_release(&fetch_text(&url)?)?.filter(|release| release.version > current))
}

/// The folder with the unpacked files of a release: the one folder inside `unpacked`, or `unpacked` itself.
fn payload_folder(unpacked: &Path) -> PathBuf {
    let entries: Vec<PathBuf> = std::fs::read_dir(unpacked)
        .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    match entries.as_slice() {
        [one] if one.is_dir() => one.clone(),
        _ => unpacked.to_path_buf(),
    }
}

/// Puts every file of `from` (folders too) into `folder`. A file that is there already (the running program
/// among them: Windows lets a running program be renamed, not overwritten) gets the name `NAME.old` first.
pub fn install_files(from: &Path, folder: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(from).map_err(|e| format!("the new files cannot be read ({e})"))?;
    for entry in entries.filter_map(Result::ok) {
        let (source, target) = (entry.path(), folder.join(entry.file_name()));
        if source.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| format!("{} cannot be made ({e})", target.display()))?;
            install_files(&source, &target)?;
            continue;
        }
        if target.exists() {
            let old = old_name(&target);
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&target, &old).map_err(|e| format!("{} cannot be replaced ({e})", target.display()))?;
        }
        // A copy, not a rename: the download folder may be on another drive.
        if let Err(e) = std::fs::copy(&source, &target) {
            // Put the old file back, so that the program still starts.
            let _ = std::fs::rename(old_name(&target), &target);
            return Err(format!("{} cannot be written ({e})", target.display()));
        }
    }
    Ok(())
}

fn old_name(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    path.with_file_name(name)
}

/// Takes away the `*.old` files an update left in `folder` (and one level under it). Called at the start.
pub fn remove_old_files(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for path in entries.filter_map(|e| e.ok().map(|e| e.path())) {
        if path.is_dir() {
            if let Ok(inner) = std::fs::read_dir(&path) {
                for file in inner.filter_map(|e| e.ok().map(|e| e.path())) {
                    if file.extension().is_some_and(|e| e == "old") {
                        let _ = std::fs::remove_file(file);
                    }
                }
            }
        } else if path.extension().is_some_and(|e| e == "old") {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Fetches `release`, checks it, unpacks it and puts it into `folder` (the folder of the program).
pub fn download_and_install(release: &Release, folder: &Path) -> Result<(), String> {
    let work = std::env::temp_dir().join(format!("joseon-update-{}", release.version));
    let _ = std::fs::remove_dir_all(&work);
    let unpacked = work.join("files");
    std::fs::create_dir_all(&unpacked).map_err(|e| format!("no room for the download ({e})"))?;
    let zip = work.join("update.zip");
    fetch_file(&release.asset_url, &zip)?;
    let bytes = std::fs::read(&zip).map_err(|e| format!("the download cannot be read ({e})"))?;
    if let Some(wanted) = &release.sha256 {
        if sha256_hex(&bytes) != *wanted {
            return Err("the download is not the file of the release (checksum)".to_string());
        }
    }
    let status = tool("tar")
        .arg("-xf")
        .arg(&zip)
        .arg("-C")
        .arg(&unpacked)
        .status()
        .map_err(|_| "tar is not there (Windows 10 or later has it)".to_string())?;
    if !status.success() {
        return Err("the download cannot be unpacked".to_string());
    }
    let payload = payload_folder(&unpacked);
    let program = if cfg!(windows) { "Joseon.exe" } else { "Joseon" };
    if !payload.join(program).is_file() {
        return Err(format!("the release has no {program}"));
    }
    install_files(&payload, folder)?;
    let _ = std::fs::remove_dir_all(&work);
    Ok(())
}

/// Runs checks and installs on a thread of its own. The window reads `status`.
#[derive(Clone, Default)]
pub struct Updater {
    status: Arc<Mutex<Status>>,
}

impl Updater {
    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn set(status: &Arc<Mutex<Status>>, value: Status) {
        if let Ok(mut status) = status.lock() {
            *status = value;
        }
    }

    /// Looks for a newer release. `quiet` = say nothing when there is none, or when the question failed (the
    /// check the program makes by itself must not bother anybody).
    pub fn check(&self, quiet: bool) {
        if self.status().is_busy() {
            return;
        }
        Self::set(&self.status, Status::Checking);
        let status = self.status.clone();
        std::thread::spawn(move || {
            let result = check(Version::current());
            crate::log::line(&format!("update check: {result:?}"));
            Self::set(
                &status,
                match result {
                    Ok(Some(release)) => Status::Available(release),
                    Ok(None) if quiet => Status::Idle,
                    Ok(None) => Status::UpToDate,
                    Err(_) if quiet => Status::Idle,
                    Err(why) => Status::Failed(why),
                },
            );
        });
    }

    /// Fetches the release that is there and puts it in the place of this program. `Ready` follows: the
    /// window then starts the new program and ends.
    pub fn install(&self) {
        let Status::Available(release) = self.status() else {
            return;
        };
        let Some(folder) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
        else {
            Self::set(
                &self.status,
                Status::Failed("the folder of the program is not known".to_string()),
            );
            return;
        };
        Self::set(&self.status, Status::Downloading(release.version));
        let status = self.status.clone();
        std::thread::spawn(move || {
            let result = if cfg!(windows) {
                download_and_install(&release, &folder)
            } else {
                Err("this build is not the Windows program".to_string())
            };
            crate::log::line(&format!("update to {}: {result:?}", release.version));
            Self::set(
                &status,
                match result {
                    Ok(()) => Status::Ready(release.version),
                    Err(why) => Status::Failed(why),
                },
            );
        });
    }
}

/// Starts the program again (the new one is in its place) with the note that an update happened.
pub fn relaunch() -> bool {
    let Ok(program) = std::env::current_exe() else {
        return false;
    };
    Command::new(program).arg(AFTER_UPDATE).spawn().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"[
      {"tag_name": "v0.0.9", "draft": true, "assets": [
        {"name": "Joseon-v0.0.9-windows-x64.zip", "browser_download_url": "https://github.com/jasas1/JoseonPC/releases/download/v0.0.9/Joseon-v0.0.9-windows-x64.zip"}]},
      {"tag_name": "v0.0.7", "prerelease": true, "assets": [
        {"name": "Joseon-v0.0.7-windows-x64.zip", "digest": "sha256:ABCDEF",
         "browser_download_url": "https://github.com/jasas1/JoseonPC/releases/download/v0.0.7/Joseon-v0.0.7-windows-x64.zip"}]},
      {"tag_name": "v0.0.8", "prerelease": true, "assets": [
        {"name": "notes.txt", "browser_download_url": "https://github.com/jasas1/JoseonPC/releases/download/v0.0.8/notes.txt"},
        {"name": "Joseon-v0.0.8-windows-x64.zip",
         "browser_download_url": "https://github.com/jasas1/JoseonPC/releases/download/v0.0.8/Joseon-v0.0.8-windows-x64.zip"}]},
      {"tag_name": "v0.1.0", "assets": [
        {"name": "Joseon-v0.1.0-windows-x64.zip", "browser_download_url": "https://example.com/Joseon-v0.1.0-windows-x64.zip"}]},
      {"tag_name": "v0.2.0", "assets": [{"name": "Joseon-v0.2.0-macos.zip", "browser_download_url": "https://github.com/jasas1/x.zip"}]},
      {"tag_name": "nightly", "assets": []}
    ]"#;

    #[test]
    fn versions_are_read_and_compared() {
        assert_eq!(Version::parse("v0.0.7"), Some(Version(0, 0, 7)));
        assert_eq!(Version::parse("1.20.3"), Some(Version(1, 20, 3)));
        for bad in ["", "v1.2", "v1.2.3.4", "nightly", "v1.x.3", "v-1.2.3"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert!(Version(0, 1, 0) > Version(0, 0, 99) && Version(1, 0, 0) > Version(0, 99, 99));
        assert_eq!(Version(0, 0, 7).to_string(), "0.0.7");
        assert!(Version::current() > Version(0, 0, 0), "the program knows its version");
    }

    #[test]
    fn the_newest_release_with_a_file_for_this_system_is_chosen() {
        // v0.0.9 is a draft, v0.1.0 comes from a foreign place, v0.2.0 has no Windows file.
        let release = newest_release(LIST).unwrap().unwrap();
        assert_eq!(release.version, Version(0, 0, 8));
        assert_eq!(release.asset_name, "Joseon-v0.0.8-windows-x64.zip");
        assert!(release.asset_url.ends_with("/v0.0.8/Joseon-v0.0.8-windows-x64.zip"));
        assert_eq!(release.sha256, None);
        let older = newest_release(&LIST.replace("v0.0.8", "v0.0.6")).unwrap().unwrap();
        assert_eq!(
            (older.version, older.sha256.as_deref()),
            (Version(0, 0, 7), Some("abcdef"))
        );
        assert_eq!(newest_release("[]"), Ok(None));
        assert_eq!(
            newest_release(r#"{"message": "Not Found"}"#),
            Err("Not Found".to_string())
        );
        assert!(newest_release("<html>").is_err());
    }

    #[test]
    fn the_menu_line_says_where_the_updater_stands() {
        let release = newest_release(LIST).unwrap().unwrap();
        assert_eq!(Status::Idle.line(), None);
        assert_eq!(Status::Available(release).line().unwrap(), "Joseon 0.0.8 is there");
        assert!(Status::UpToDate.line().unwrap().ends_with("is the newest version"));
        assert_eq!(
            Status::Failed("no answer".to_string()).line().unwrap(),
            "Update failed: no answer"
        );
        assert!(Status::Checking.is_busy() && !Status::UpToDate.is_busy());
    }

    #[test]
    fn new_files_take_the_place_of_the_old_ones() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let root = std::env::temp_dir().join(format!("joseon-update-test-{nanos}"));
        let (new, folder) = (root.join("unpacked/Joseon-v9-windows-x64"), root.join("app"));
        std::fs::create_dir_all(new.join("licenses")).unwrap();
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(new.join("Joseon.exe"), "new program").unwrap();
        std::fs::write(new.join("licenses/a.md"), "new text").unwrap();
        std::fs::write(folder.join("Joseon.exe"), "old program").unwrap();
        std::fs::write(folder.join("Joseon.exe.old"), "older program").unwrap();
        std::fs::write(folder.join("settings-of-the-user.txt"), "mine").unwrap();
        assert_eq!(payload_folder(&root.join("unpacked")), new);
        install_files(&new, &folder).unwrap();
        let read = |name: &str| std::fs::read_to_string(folder.join(name)).unwrap();
        assert_eq!(read("Joseon.exe"), "new program");
        assert_eq!(read("Joseon.exe.old"), "old program");
        assert_eq!(read("licenses/a.md"), "new text");
        assert_eq!(read("settings-of-the-user.txt"), "mine", "other files stay");
        // A second update over the first one.
        std::fs::write(new.join("licenses/a.md"), "newer text").unwrap();
        install_files(&new, &folder).unwrap();
        assert_eq!(read("licenses/a.md"), "newer text");
        remove_old_files(&folder);
        assert!(!folder.join("Joseon.exe.old").exists() && !folder.join("licenses/a.md.old").exists());
        assert!(folder.join("Joseon.exe").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The whole way with the real tools: a zip as the release workflow makes it, fetched from a file address.
    #[test]
    fn a_release_is_fetched_checked_unpacked_and_put_in_place() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let root = std::env::temp_dir().join(format!("joseon-update-whole-{nanos}"));
        let (dist, folder) = (root.join("dist"), root.join("app"));
        let inside = dist.join("Joseon-v9.9.9-windows-x64");
        std::fs::create_dir_all(inside.join("licenses")).unwrap();
        std::fs::create_dir_all(&folder).unwrap();
        let program = if cfg!(windows) { "Joseon.exe" } else { "Joseon" };
        std::fs::write(inside.join(program), "the new program").unwrap();
        std::fs::write(inside.join("licenses/notice.md"), "text").unwrap();
        std::fs::write(folder.join(program), "the old program").unwrap();
        let zip = root.join("Joseon-v9.9.9-windows-x64.zip");
        let made = tool("tar")
            .arg("-a")
            .arg("-cf")
            .arg(&zip)
            .arg("-C")
            .arg(&dist)
            .arg("Joseon-v9.9.9-windows-x64")
            .status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("skipped: this machine has no tar that writes a zip");
            return;
        }
        let path = zip.to_string_lossy().replace('\\', "/");
        let url = if path.starts_with('/') {
            format!("file://{path}")
        } else {
            format!("file:///{path}")
        };
        let mut release = Release {
            version: Version(9, 9, 9),
            asset_name: "Joseon-v9.9.9-windows-x64.zip".to_string(),
            asset_url: url,
            sha256: Some("00".repeat(32)),
        };
        // A file that is not the one of the release is not put in place.
        let refused = download_and_install(&release, &folder).unwrap_err();
        assert!(refused.contains("checksum"), "{refused}");
        assert_eq!(
            std::fs::read_to_string(folder.join(program)).unwrap(),
            "the old program"
        );
        release.sha256 = Some(sha256_hex(&std::fs::read(&zip).unwrap()));
        download_and_install(&release, &folder).unwrap();
        assert_eq!(
            std::fs::read_to_string(folder.join(program)).unwrap(),
            "the new program"
        );
        assert_eq!(
            std::fs::read_to_string(folder.join("licenses/notice.md")).unwrap(),
            "text"
        );
        assert!(folder.join(format!("{program}.old")).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_checksum_is_sha_256_in_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
