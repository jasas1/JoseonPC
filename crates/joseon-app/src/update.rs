//! Updates: look for a newer release, fetch it, put it in the place of this program, start it.
//!
//! The release list is the public one of the project on GitHub. The program asks for it once after the
//! start and once a day (a setting), and when the user says so. Nothing is sent but the request itself.
//! Installing is a separate step, and only when the user asks. A release is installed only when GitHub
//! gives a SHA-256 and the file matches it. On Windows, `curl` and `tar` are the copies in the system
//! directory. Redirects are followed only when the next host is on the list below.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// The release list. `JOSEON_UPDATE_URL` may name another https address on [`ALLOWED_HOSTS`] (for a test).
pub const RELEASES_URL: &str = "https://api.github.com/repos/jasas1/JoseonPC/releases?per_page=10";
/// A file of a release may only come from these places. The download itself may then be redirected to
/// [`ALLOWED_HOSTS`].
const DOWNLOAD_PREFIXES: [&str; 2] = ["https://github.com/jasas1/", "https://objects.githubusercontent.com/"];
/// Hosts an update request may use. Exact match, https only. `release-assets.githubusercontent.com` is
/// where a GitHub release file is redirected.
const ALLOWED_HOSTS: [&str; 4] = [
    "api.github.com",
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];
/// `curl` is not told to follow redirects. Each hop is checked, and this many are enough.
const MAX_REDIRECTS: u32 = 5;
/// Passed to `curl` so it neither follows a redirect nor speaks anything but https.
const CURL_REDIRECT_ARGS: &[&str] = &["--max-redirs", "0", "--proto", "=https", "--proto-redir", "=https"];
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
        // No digest, or a digest that is not 64 hex digits: this release cannot be installed.
        let Some(sha256) = asset["digest"].as_str().and_then(sha256_from_digest_field) else {
            continue;
        };
        if best.as_ref().is_none_or(|b| version > b.version) {
            best = Some(Release {
                version,
                asset_name: name.to_string(),
                asset_url: url.to_string(),
                sha256: Some(sha256),
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

/// `curl` or `tar`, with no console window. On Windows the program is the one in the system directory,
/// never a name that another directory on `PATH` could answer.
fn tool(name: &str) -> Result<Command, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new(system_tool(name)?);
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        Ok(command)
    }
    #[cfg(not(windows))]
    {
        // The tests unpack a zip with tar. The Windows build does not reach this branch.
        Ok(Command::new(name))
    }
}

/// The Windows system directory, from `GetSystemDirectoryW` (not a hard-coded drive).
#[cfg(windows)]
fn system_directory() -> Result<PathBuf, String> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    unsafe {
        let mut buf = vec![0u16; 261];
        let mut len = GetSystemDirectoryW(Some(buf.as_mut_slice()));
        if len == 0 {
            return Err("the Windows system directory is not known".to_string());
        }
        if len as usize >= buf.len() {
            buf.resize(len as usize, 0);
            len = GetSystemDirectoryW(Some(buf.as_mut_slice()));
            if len == 0 || len as usize >= buf.len() {
                return Err("the Windows system directory is not known".to_string());
            }
        }
        Ok(PathBuf::from(std::ffi::OsString::from_wide(&buf[..len as usize])))
    }
}

/// `name.exe` in the Windows system directory, and only there.
#[cfg(windows)]
fn system_tool(name: &str) -> Result<PathBuf, String> {
    let path = system_directory()?.join(format!("{name}.exe"));
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!("{name} is not in the Windows system directory"))
    }
}

fn fetch_text(url: &str) -> Result<String, String> {
    let bytes = fetch_bytes(url, "20", true)?;
    String::from_utf8(bytes).map_err(|_| "the release list is not text".to_string())
}

fn fetch_file(url: &str, to: &Path) -> Result<(), String> {
    // A file address exists only in tests, so the unpacking test needs no server. A release build
    // has no such path: `fetch_bytes` allows https on [`ALLOWED_HOSTS`] and nothing else.
    #[cfg(test)]
    if let Some(copied) = copy_file_url(url, to) {
        return copied;
    }
    let bytes = fetch_bytes(url, "300", false)?;
    std::fs::write(to, bytes).map_err(|e| format!("the download cannot be written ({e})"))
}

/// One https request. A redirect is followed only when [`next_hop`] accepts it.
fn fetch_bytes(url: &str, max_time: &str, api: bool) -> Result<Vec<u8>, String> {
    let mut current = url.to_string();
    let mut redirects = 0;
    loop {
        if !allowed_update_url(&current) {
            return Err(format!("refusing an update from {current}"));
        }
        let scratch = std::env::temp_dir().join(format!("joseon-fetch-{}-{redirects}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).map_err(|e| format!("no room for the download ({e})"))?;
        let body = scratch.join("body");
        let headers_path = scratch.join("headers");
        let exchanged = curl_exchange(&current, &body, &headers_path, max_time, api);
        let decided = match exchanged {
            Ok((code, headers)) => next_hop(code, location_header(&headers).as_deref(), &current),
            Err(why) => Err(why),
        };
        match decided {
            Ok(None) => {
                let bytes = std::fs::read(&body).map_err(|e| format!("the download cannot be read ({e})"));
                let _ = std::fs::remove_dir_all(&scratch);
                return bytes;
            }
            Ok(Some(next)) => {
                let _ = std::fs::remove_dir_all(&scratch);
                redirects += 1;
                if redirects > MAX_REDIRECTS {
                    return Err("too many redirects".to_string());
                }
                current = next;
            }
            Err(why) => {
                let _ = std::fs::remove_dir_all(&scratch);
                return Err(why);
            }
        }
    }
}

fn curl_exchange(
    url: &str,
    body: &Path,
    headers_path: &Path,
    max_time: &str,
    api: bool,
) -> Result<(u16, String), String> {
    let mut command = tool("curl")?;
    command.args(["-sS", "--max-time", max_time]);
    command.args(CURL_REDIRECT_ARGS);
    command.arg("-A").arg(format!("Joseon/{}", Version::current()));
    if api {
        command.args(["-H", "Accept: application/vnd.github+json"]);
    }
    command.arg("-D").arg(headers_path);
    command.arg("-o").arg(body);
    command.args(["-w", "%{http_code}", "--"]);
    command.arg(url);
    let output = command
        .output()
        .map_err(|_| "curl is not there (Windows 10 or later has it)".to_string())?;
    if !output.status.success() {
        return Err("no answer from the update server".to_string());
    }
    let code = String::from_utf8_lossy(&output.stdout);
    let code = code
        .trim()
        .parse::<u16>()
        .map_err(|_| "the update server gave no status".to_string())?;
    let headers = std::fs::read(headers_path).unwrap_or_default();
    Ok((code, String::from_utf8_lossy(&headers).into_owned()))
}

/// `true` when `url` is https, has no user or odd port, and its host is exactly one of [`ALLOWED_HOSTS`].
fn allowed_update_url(url: &str) -> bool {
    parse_https(url).is_ok_and(|(host, _)| ALLOWED_HOSTS.contains(&host.as_str()))
}

/// Host and path (the path starts with `/` and keeps the query). Fragments are dropped.
fn parse_https(url: &str) -> Result<(String, String), String> {
    if url.chars().any(|c| c.is_control() || c.is_whitespace() || c == '\\') {
        return Err("the address is not usable".to_string());
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return Err("the address is not https".to_string());
    };
    if rest.is_empty() {
        return Err("the address is not usable".to_string());
    }
    let (authority, path) = if let Some((authority, after)) = rest.split_once('/') {
        let path = format!("/{after}");
        let path = path.split('#').next().unwrap_or("/").to_string();
        (authority, path)
    } else if let Some((authority, query)) = rest.split_once('?') {
        let query = query.split('#').next().unwrap_or("");
        (authority, format!("/?{query}"))
    } else {
        (rest.split('#').next().unwrap_or(rest), "/".to_string())
    };
    if authority.is_empty() || authority.contains('@') || authority.contains('[') {
        return Err("the address is not usable".to_string());
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => (host, Some(port)),
        Some(_) => return Err("the address is not usable".to_string()),
        None => (authority, None),
    };
    if host.is_empty() || port.is_some_and(|p| p != "443") {
        return Err("the address is not usable".to_string());
    }
    Ok((host.to_ascii_lowercase(), path))
}

fn location_header(headers: &str) -> Option<String> {
    for line in headers.split('\n') {
        let line = line.trim_end_matches('\r');
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("location") {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// The address a redirect asks for, still relative to `current` when it is not absolute.
fn resolve_redirect(current: &str, location: &str) -> Result<String, String> {
    let location = location.trim();
    if location.is_empty() || location.chars().any(|c| c.is_control() || c == '\\') {
        return Err("the redirect is not usable".to_string());
    }
    if location.starts_with("https://") || location.starts_with("http://") {
        return Ok(location.to_string());
    }
    let (host, path) = parse_https(current)?;
    if let Some(rest) = location.strip_prefix("//") {
        return Ok(format!("https://{rest}"));
    }
    if location.starts_with('/') {
        return Ok(format!("https://{host}{location}"));
    }
    if location.starts_with('?') {
        let path_only = path.split('?').next().unwrap_or("/");
        return Ok(format!("https://{host}{path_only}{location}"));
    }
    let path_only = path.split('?').next().unwrap_or("/");
    let dir = match path_only.rfind('/') {
        Some(i) => &path_only[..=i],
        None => "/",
    };
    Ok(format!("https://{host}{dir}{location}"))
}

/// The next address, or an error when it leaves [`ALLOWED_HOSTS`].
fn accept_redirect(current: &str, location: &str) -> Result<String, String> {
    let next = resolve_redirect(current, location)?;
    if allowed_update_url(&next) {
        Ok(next)
    } else {
        Err(format!("refusing a redirect to {next}"))
    }
}

/// `Ok(None)` = the body is the file. `Ok(Some)` = fetch that address next.
fn next_hop(code: u16, location: Option<&str>, current: &str) -> Result<Option<String>, String> {
    match code {
        200 => Ok(None),
        301 | 302 | 303 | 307 | 308 => {
            let location = location.ok_or_else(|| "the redirect has no address".to_string())?;
            accept_redirect(current, location).map(Some)
        }
        _ => Err(format!("the update server answered {code}")),
    }
}

/// Copies `file://...` in tests. `None` when `url` is not a file address.
#[cfg(test)]
fn copy_file_url(url: &str, to: &Path) -> Option<Result<(), String>> {
    let rest = url.strip_prefix("file://")?;
    // `file:///C:/...` keeps one slash in front of the drive letter. A Unix path does not.
    let path = if rest.len() >= 3 && rest.as_bytes()[0] == b'/' && rest.as_bytes()[2] == b':' {
        &rest[1..]
    } else {
        rest
    };
    Some(
        std::fs::copy(path, to)
            .map(|_| ())
            .map_err(|e| format!("the download cannot be read ({e})")),
    )
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// 64 hex digits, either case.
fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `sha256:` plus 64 hex digits, as GitHub writes a release asset digest. Lower case.
fn sha256_from_digest_field(digest: &str) -> Option<String> {
    let hex = digest.strip_prefix("sha256:")?.to_ascii_lowercase();
    is_sha256_hex(&hex).then_some(hex)
}

/// The digest an install may use, or an error when there is none.
fn canonical_digest(sha256: &Option<String>) -> Result<String, String> {
    match sha256 {
        Some(hex) if is_sha256_hex(hex) => Ok(hex.to_ascii_lowercase()),
        _ => Err("the release has no SHA-256 digest".to_string()),
    }
}

fn check_digest(wanted: &str, bytes: &[u8]) -> Result<(), String> {
    let wanted = canonical_digest(&Some(wanted.to_string()))?;
    if sha256_hex(bytes) != wanted {
        return Err("the download is not the file of the release (checksum)".to_string());
    }
    Ok(())
}

/// `JOSEON_REQUIRE_SIGNATURE=1` at compile time. The release workflow sets it only when SignPath
/// signing is configured, so a signed program checks the next update. An ordinary build does not:
/// until a signed release exists, requiring a signature would refuse every update.
fn requires_authenticode() -> bool {
    option_env!("JOSEON_REQUIRE_SIGNATURE").is_some_and(|value| value == "1")
}

/// Authenticode via `WinVerifyTrust`, with no dialog. The chain is checked for revocation.
#[cfg(windows)]
fn verify_authenticode(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Security::WinTrust::{
        WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
        WTD_CHOICE_FILE, WTD_REVOCATION_CHECK_CHAIN, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
        WTD_UI_NONE,
    };

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: u32::try_from(std::mem::size_of::<WINTRUST_FILE_INFO>()).unwrap_or(0),
        pcwszFilePath: PCWSTR(wide.as_ptr()),
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let mut data = WINTRUST_DATA {
        cbStruct: u32::try_from(std::mem::size_of::<WINTRUST_DATA>()).unwrap_or(0),
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_REVOCATION_CHECK_CHAIN,
        ..Default::default()
    };
    let status = unsafe {
        WinVerifyTrust(
            HWND(std::ptr::null_mut()),
            &mut action,
            &mut data as *mut WINTRUST_DATA as *mut _,
        )
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            HWND(std::ptr::null_mut()),
            &mut action,
            &mut data as *mut WINTRUST_DATA as *mut _,
        );
    }
    if status == 0 {
        Ok(())
    } else {
        Err(format!(
            "the new program has no valid Authenticode signature ({:#010x})",
            status as u32
        ))
    }
}

#[cfg(not(windows))]
fn verify_authenticode(_path: &Path) -> Result<(), String> {
    Err("Authenticode can only be checked on Windows".to_string())
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
    // Before any download: a release with no digest is not installed.
    let wanted = canonical_digest(&release.sha256)?;
    let work = std::env::temp_dir().join(format!("joseon-update-{}", release.version));
    let _ = std::fs::remove_dir_all(&work);
    let unpacked = work.join("files");
    std::fs::create_dir_all(&unpacked).map_err(|e| format!("no room for the download ({e})"))?;
    let zip = work.join("update.zip");
    fetch_file(&release.asset_url, &zip)?;
    let bytes = std::fs::read(&zip).map_err(|e| format!("the download cannot be read ({e})"))?;
    check_digest(&wanted, &bytes)?;
    let status = tool("tar")?
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
    let program_path = payload.join(program);
    if !program_path.is_file() {
        return Err(format!("the release has no {program}"));
    }
    if requires_authenticode() {
        verify_authenticode(&program_path)?;
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

    /// 64 hex digits. A shorter digest, such as "ABCDEF", does not count.
    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    const LIST: &str = r#"[
      {"tag_name": "v0.0.9", "draft": true, "assets": [
        {"name": "Joseon-v0.0.9-windows-x64.zip", "digest": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
         "browser_download_url": "https://github.com/jasas1/JoseonPC/releases/download/v0.0.9/Joseon-v0.0.9-windows-x64.zip"}]},
      {"tag_name": "v0.0.7", "prerelease": true, "assets": [
        {"name": "Joseon-v0.0.7-windows-x64.zip", "digest": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
         "browser_download_url": "https://github.com/jasas1/JoseonPC/releases/download/v0.0.7/Joseon-v0.0.7-windows-x64.zip"}]},
      {"tag_name": "v0.0.8", "prerelease": true, "assets": [
        {"name": "notes.txt", "browser_download_url": "https://github.com/jasas1/JoseonPC/releases/download/v0.0.8/notes.txt"},
        {"name": "Joseon-v0.0.8-windows-x64.zip",
         "browser_download_url": "https://github.com/jasas1/JoseonPC/releases/download/v0.0.8/Joseon-v0.0.8-windows-x64.zip"}]},
      {"tag_name": "v0.1.0", "assets": [
        {"name": "Joseon-v0.1.0-windows-x64.zip", "digest": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
         "browser_download_url": "https://example.com/Joseon-v0.1.0-windows-x64.zip"}]},
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
        // v0.0.9 is a draft, v0.0.8 has no digest, v0.1.0 comes from a foreign place, v0.2.0 has no Windows file.
        let release = newest_release(LIST).unwrap().unwrap();
        assert_eq!(release.version, Version(0, 0, 7));
        assert_eq!(release.asset_name, "Joseon-v0.0.7-windows-x64.zip");
        assert!(release.asset_url.ends_with("/v0.0.7/Joseon-v0.0.7-windows-x64.zip"));
        assert_eq!(release.sha256.as_deref(), Some(DIGEST));
        // A short digest does not count, so nothing is left.
        assert_eq!(newest_release(&LIST.replace(DIGEST, "abcdef")), Ok(None));
        // The same asset with a full digest is chosen, and the hex is stored in lower case.
        let with_digest = LIST.replacen(
            "\"name\": \"Joseon-v0.0.8-windows-x64.zip\"",
            &format!("\"name\": \"Joseon-v0.0.8-windows-x64.zip\", \"digest\": \"sha256:{DIGEST}\""),
            1,
        );
        let newer = newest_release(&with_digest).unwrap().unwrap();
        assert_eq!(
            (newer.version, newer.sha256.as_deref()),
            (Version(0, 0, 8), Some(DIGEST))
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
        assert_eq!(Status::Available(release).line().unwrap(), "Joseon 0.0.7 is there");
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
        let made = tool("tar").and_then(|mut command| {
            command
                .arg("-a")
                .arg("-cf")
                .arg(&zip)
                .arg("-C")
                .arg(&dist)
                .arg("Joseon-v9.9.9-windows-x64")
                .status()
                .map_err(|e| e.to_string())
        });
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
        assert!(check_digest(&sha256_hex(b"abc"), b"abc").is_ok());
        let wrong = check_digest(&"ab".repeat(32), b"abc").unwrap_err();
        assert!(wrong.contains("checksum"), "{wrong}");
        assert!(check_digest("abcdef", b"abc").unwrap_err().contains("no SHA-256"));
        assert!(canonical_digest(&None).unwrap_err().contains("no SHA-256"));
        assert_eq!(
            sha256_from_digest_field(&format!("sha256:{}", DIGEST.to_ascii_uppercase())).as_deref(),
            Some(DIGEST)
        );
    }

    #[test]
    fn a_release_without_a_digest_is_not_installed() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let folder = std::env::temp_dir().join(format!("joseon-update-nodigest-{nanos}"));
        std::fs::create_dir_all(&folder).unwrap();
        let program = if cfg!(windows) { "Joseon.exe" } else { "Joseon" };
        std::fs::write(folder.join(program), "the old program").unwrap();
        let mut release = Release {
            version: Version(8, 8, 8),
            asset_name: "Joseon.zip".to_string(),
            asset_url: "https://evil.example/Joseon.zip".to_string(),
            sha256: None,
        };
        let missing = download_and_install(&release, &folder).unwrap_err();
        assert!(missing.contains("no SHA-256"), "{missing}");
        release.sha256 = Some("abcdef".to_string());
        let short = download_and_install(&release, &folder).unwrap_err();
        assert!(short.contains("no SHA-256"), "{short}");
        assert_eq!(
            std::fs::read_to_string(folder.join(program)).unwrap(),
            "the old program"
        );
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn an_update_from_outside_the_allow_list_is_refused() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let folder = std::env::temp_dir().join(format!("joseon-update-foreign-{nanos}"));
        std::fs::create_dir_all(&folder).unwrap();
        let program = if cfg!(windows) { "Joseon.exe" } else { "Joseon" };
        std::fs::write(folder.join(program), "the old program").unwrap();
        let release = Release {
            version: Version(7, 7, 7),
            asset_name: "Joseon.zip".to_string(),
            asset_url: "https://evil.example/Joseon.zip".to_string(),
            sha256: Some(DIGEST.to_string()),
        };
        let refused = download_and_install(&release, &folder).unwrap_err();
        assert!(refused.contains("refusing"), "{refused}");
        assert_eq!(
            std::fs::read_to_string(folder.join(program)).unwrap(),
            "the old program"
        );
        let _ = std::fs::remove_dir_all(&folder);
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("joseon-update-7.7.7"));
    }

    #[test]
    fn redirects_stay_on_the_github_hosts() {
        assert!(allowed_update_url(RELEASES_URL));
        assert!(allowed_update_url(
            "https://github.com/jasas1/JoseonPC/releases/download/v0.0.11/a.zip"
        ));
        assert!(allowed_update_url(
            "https://objects.githubusercontent.com/github-production-release-asset/x"
        ));
        assert!(allowed_update_url(
            "https://release-assets.githubusercontent.com/github-production-release-asset/x"
        ));
        assert!(allowed_update_url("https://github.com:443/jasas1/JoseonPC"));
        for refused in [
            "https://evil.example/a.zip",
            "https://github.com.evil/a.zip",
            "https://user:pw@github.com/a.zip",
            "http://github.com/a.zip",
            "https://github.com:8443/a.zip",
            "file:///tmp/a.zip",
            "https://github.com.",
        ] {
            assert!(!allowed_update_url(refused), "{refused}");
        }

        let from = "https://github.com/jasas1/JoseonPC/releases/download/v0.0.11/a.zip";
        assert!(accept_redirect(from, "https://release-assets.githubusercontent.com/x?a=1").is_ok());
        assert!(accept_redirect(from, "https://objects.githubusercontent.com/x").is_ok());
        assert_eq!(
            accept_redirect(from, "/jasas1/other").unwrap(),
            "https://github.com/jasas1/other"
        );
        for location in [
            "https://evil.example/a.zip",
            "http://github.com/a.zip",
            "https://user@github.com/a.zip",
            "https://github.com.evil/a.zip",
            "//evil.example/a",
        ] {
            let refused = accept_redirect(from, location).unwrap_err();
            assert!(refused.contains("refusing"), "{location}: {refused}");
        }
        assert_eq!(next_hop(200, None, from).unwrap(), None);
        assert!(next_hop(302, Some("https://evil.example/a"), from).is_err());
        assert_eq!(
            next_hop(302, Some("https://release-assets.githubusercontent.com/a"), from)
                .unwrap()
                .as_deref(),
            Some("https://release-assets.githubusercontent.com/a")
        );
        assert!(next_hop(302, None, from).unwrap_err().contains("no address"));
        assert!(next_hop(404, None, from).is_err());
        assert_eq!(
            location_header("HTTP/1.1 302 Found\r\nLocation: https://objects.githubusercontent.com/a\r\n").as_deref(),
            Some("https://objects.githubusercontent.com/a")
        );
        assert!(!CURL_REDIRECT_ARGS
            .iter()
            .any(|arg| *arg == "-L" || *arg == "--location"));
        assert!(CURL_REDIRECT_ARGS.contains(&"--max-redirs"));
    }

    #[test]
    fn a_normal_build_does_not_require_an_authenticode_signature() {
        assert!(!requires_authenticode());
    }

    #[cfg(windows)]
    #[test]
    fn curl_and_tar_come_from_the_system_directory() {
        let dir = system_directory().unwrap();
        for name in ["curl", "tar"] {
            let path = system_tool(name).unwrap();
            assert!(path.is_absolute(), "{}", path.display());
            assert!(path.is_file(), "{}", path.display());
            assert!(path.starts_with(&dir), "{}", path.display());
            assert_eq!(path.file_name().unwrap(), std::ffi::OsStr::new(&format!("{name}.exe")));
        }
    }
}
