//! Self-updater for the exe: the `update` console command.
//!
//! Flow: ask the GitHub API for this (public) repo's latest release → compare
//! against the running version → download the installable Windows asset next
//! to the running exe → write a small updater batch file →
//! spawn it detached → the console quits gracefully; the batch waits for our
//! PID to exit, swaps the new exe over the old one, restarts it with the same
//! arguments, and deletes itself.
//!
//! No HTTP-client crate is pulled in: like [`crate::update_check`], this
//! reuses the crate's existing TLS stack (`rustls` + `ring` + the OS trust
//! store) and speaks raw HTTP/1.1, including redirect-following and
//! chunked-body decoding for the API responses.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::update_check;

/// `owner/repo` whose releases carry the Windows assets. The repo is public,
/// so the API calls below work with no token at all; an optional token
/// (`QUICMIC_GITHUB_TOKEN` env var, or `github_token.txt` next to the exe)
/// only raises the GitHub API rate limit and is never required.
const REPO: &str = "crtGhoul/quicmic-zero-ui";

/// Overall budgets so a wedged network can never hang the console forever.
const API_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// Where the token lives if not given via env.
const TOKEN_FILE: &str = "github_token.txt";

/// A release worth installing: its tag plus the installable asset's API URL.
pub struct ReleaseInfo {
    pub tag: String,
    pub asset_name: String,
    pub asset_api_url: String,
}

/// Optional GitHub token: env var wins, then `github_token.txt` next to the
/// exe (first line). Only used to raise API rate limits on `api.github.com`;
/// updates work without it. The value is never logged.
pub fn github_token() -> Option<String> {
    if let Ok(t) = std::env::var("QUICMIC_GITHUB_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    token_from_file(&token_file_path())
}

fn token_file_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(TOKEN_FILE)))
        .unwrap_or_else(|| PathBuf::from(TOKEN_FILE))
}

pub(crate) fn token_from_file(path: &Path) -> Option<String> {
    let line = std::fs::read_to_string(path)
        .ok()?
        .lines()
        .next()?
        .trim()
        .to_string();
    (!line.is_empty()).then_some(line)
}

/// Run the whole update flow, printing progress. Returns `true` when a new
/// version was staged and the caller should quit so the updater batch can
/// swap the exe and restart it.
pub async fn run_update(
    #[cfg_attr(not(windows), allow(unused_variables))] data_dir: &std::path::Path,
) -> anyhow::Result<bool> {
    // Platform gate first: self-update only exists for the Windows exe, so on
    // other platforms bail before any network work instead of downloading an
    // exe we would only discard. (`cfg!` rather than `#[cfg]` so the rest of
    // the function still compiles — and stays dead-code-warning-free — on
    // every platform.)
    if !cfg!(windows) {
        println!("Self-update is only wired up for the Windows exe.");
        return Ok(false);
    }

    let token = github_token();

    println!("Checking for updates…");
    let rel = tokio::time::timeout(API_TIMEOUT, latest_release(token.as_deref()))
        .await
        .map_err(|_| anyhow::anyhow!("GitHub API timed out"))??;

    let current = env!("CARGO_PKG_VERSION");
    let newer = match (
        update_check::parse_version(&rel.tag),
        update_check::parse_version(current),
    ) {
        (Some(latest), Some(cur)) => latest > cur,
        _ => false,
    };
    if !newer {
        println!("You're already on the latest ({current}).");
        return Ok(false);
    }

    println!(
        "New release {} found — downloading {}…",
        rel.tag, rel.asset_name
    );
    let exe = std::env::current_exe()?;
    let is_msi = rel.asset_name.to_ascii_lowercase().ends_with(".msi");
    // An .msi isn't swapped over the running exe like the .exe payload — it
    // goes to %TEMP% and Windows Installer takes over from there.
    let dest = if is_msi {
        std::env::temp_dir().join(format!("quicmic-update-{}.msi", rel.tag))
    } else {
        exe.with_extension("exe.new")
    };
    let bytes = tokio::time::timeout(
        DOWNLOAD_TIMEOUT,
        download_asset(&rel.asset_api_url, token.as_deref(), &dest),
    )
    .await
    .map_err(|_| anyhow::anyhow!("download timed out"))??;
    println!("Downloaded {:.1} MB.", bytes as f64 / 1_048_576.0);

    #[cfg(windows)]
    {
        if is_msi {
            stage_msi_install(&dest)?;
            // Record the staged update so the next launch can verify it
            // actually landed. Previously a blocked/denied installer left the
            // user on the old version with no message at all.
            write_pending_marker(data_dir, &rel.tag);
            println!(
                "Update staged — Windows Installer is taking over. If Windows asks for \
                 permission, accept it: the installer cannot replace QuicMic without it. \
                 QuicMic will now close so the installer can replace it; relaunch QuicMic \
                 from the Start Menu when the installer finishes."
            );
        } else {
            stage_self_update(&dest)?;
            println!("Update staged — restarting into the new version…");
        }
        Ok(true)
    }
    // Non-Windows never reaches here (the `cfg!` gate above returned), but the
    // arm must exist so the function typechecks on every platform.
    #[cfg(not(windows))]
    {
        let _ = dest;
        Ok(false)
    }
}

/// Query the repo's latest release and pick its installable Windows asset.
/// The token is optional: when present it is sent only to `api.github.com`
/// (higher rate limit); the public release needs no auth.
async fn latest_release(token: Option<&str>) -> anyhow::Result<ReleaseInfo> {
    let mut headers = vec![
        ("Accept", "application/vnd.github+json".to_string()),
        ("X-GitHub-Api-Version", "2022-11-28".to_string()),
    ];
    if let Some(t) = token {
        headers.push(("Authorization", format!("Bearer {t}")));
    }
    let resp = https_get(
        "api.github.com",
        &format!("/repos/{REPO}/releases/latest"),
        &headers
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect::<Vec<_>>(),
    )
    .await?;
    match resp.status {
        200 => {}
        401 => anyhow::bail!(
            "GitHub rejected the optional token (401) — remove/refresh QUICMIC_GITHUB_TOKEN, or unset it (public releases need no token)"
        ),
        403 => anyhow::bail!(
            "GitHub API rate-limited this network (403) — try again later, or set QUICMIC_GITHUB_TOKEN for a higher limit"
        ),
        404 => anyhow::bail!("no published release found in {REPO} (404)"),
        s => anyhow::bail!("GitHub API returned status {s}"),
    }
    let body = decode_body(&resp)?;
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| anyhow::anyhow!("couldn't parse GitHub's response: {e}"))?;
    let tag = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    if tag.is_empty() {
        anyhow::bail!("GitHub's response had no tag_name");
    }
    let (asset_name, asset_api_url) = v
        .get("assets")
        .and_then(|a| a.as_array())
        .and_then(|assets| pick_install_asset(assets))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "latest release {tag} has no installable Windows asset (looked for a setup .exe, an .msi installer, or a bare .exe for this PC's architecture) — zip-only releases can't self-update; download the installer manually from https://github.com/{REPO}/releases/latest"
            )
        })?;
    Ok(ReleaseInfo {
        tag,
        asset_name,
        asset_api_url,
    })
}

/// Arch tokens a Windows asset name may carry for the running PC, most
/// specific first (cargo-dist uses the target triple, e.g. `x86_64`).
fn arch_tokens() -> (&'static str, &'static str) {
    match std::env::consts::ARCH {
        "x86_64" => ("x86_64", "x64"),
        "aarch64" => ("aarch64", "arm64"),
        _ => ("", ""),
    }
}

fn name_matches_arch(lower_name: &str) -> bool {
    let (a, b) = arch_tokens();
    (!a.is_empty() && lower_name.contains(a)) || (!b.is_empty() && lower_name.contains(b))
}

/// Pick the release asset to install. Pure, so it is unit-testable. Returns
/// `(name, api_url)`.
///
/// cargo-dist ships archives plus a Windows MSI installer — not the bare
/// portable `.exe` this updater swaps in place. Preference order:
/// 1. a setup `.exe` for this PC's architecture (`…-setup.exe`),
/// 2. an `.msi` installer for this architecture (`quicmic-<triple>.msi`),
/// 3. a bare `.exe` for this architecture,
/// 4. any `.msi` (last resort when arch is unmarked),
/// 5. any remaining bare `.exe`.
///
/// A name like `quicmic.exe.zip` never qualifies — archives can't be swapped
/// over the running exe, and a zip-only release yields `None` so the caller
/// can point the user at the manual download instead of staging garbage.
fn pick_install_asset(assets: &[serde_json::Value]) -> Option<(String, String)> {
    let named = |want: &dyn Fn(&str) -> bool| -> Option<(String, String)> {
        assets.iter().find_map(|a| {
            let name = a.get("name")?.as_str()?;
            if !want(&name.to_ascii_lowercase()) {
                return None;
            }
            let url = a.get("url")?.as_str()?.to_string();
            Some((name.to_string(), url))
        })
    };
    named(&|n| n.contains("setup") && n.ends_with(".exe") && name_matches_arch(n))
        .or_else(|| named(&|n| n.ends_with(".msi") && name_matches_arch(n)))
        .or_else(|| named(&|n| n.ends_with(".exe") && !n.contains("setup") && name_matches_arch(n)))
        .or_else(|| named(&|n| n.ends_with(".msi")))
        .or_else(|| named(&|n| n.ends_with(".exe") && !n.contains("setup")))
}

/// Cheap integrity gate for a downloaded Windows executable: every PE starts
/// with the `MZ` magic. Catches truncated downloads and mis-served error pages
/// (an HTML 404 body saved as the "exe") before they can be staged.
fn looks_like_pe(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == b'M' && bytes[1] == b'Z'
}

/// Cheap integrity gate for a downloaded `.msi`: every MSI is an OLE2
/// compound file, which starts with this 8-byte signature.
fn looks_like_msi(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && bytes[..8] == [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]
}

/// Download an asset API URL, following redirects (the API 302s to a signed
/// `objects.githubusercontent.com` URL). The token is only sent to
/// `api.github.com`, never to the redirect target. The asset is small enough
/// (~8 MB) to hold in memory, then written to `dest` in one go.
async fn download_asset(api_url: &str, token: Option<&str>, dest: &Path) -> anyhow::Result<u64> {
    let mut url = api_url.to_string();
    for _ in 0..5 {
        let (host, path) = split_url(&url)?;
        let mut headers = vec![("Accept", "application/octet-stream".to_string())];
        if host == "api.github.com" {
            if let Some(t) = token {
                headers.push(("Authorization", format!("Bearer {t}")));
            }
        }
        let resp = https_get(&host, &path, &headers).await?;
        match resp.status {
            200 => {
                let body = decode_body(&resp)?;
                // Never stage something that isn't an executable: a truncated
                // download or a mis-served error page must fail here, loudly,
                // rather than replace the working exe with garbage.
                if !looks_like_pe(&body) && !looks_like_msi(&body) {
                    anyhow::bail!(
                        "downloaded asset is not a Windows executable or installer ({} bytes, no MZ/MSI header) — not staging it",
                        body.len()
                    );
                }
                std::fs::write(dest, &body)?;
                return Ok(body.len() as u64);
            }
            301 | 302 | 303 | 307 | 308 => {
                url = resp
                    .header("location")
                    .ok_or_else(|| anyhow::anyhow!("redirect without Location"))?
                    .to_string();
            }
            401 => anyhow::bail!("GitHub rejected the optional token (401) — remove it or refresh it; public releases need no token"),
            s => anyhow::bail!("download failed with status {s}"),
        }
    }
    anyhow::bail!("too many redirects")
}

/// Split `https://host/path…` into `(host, path)`.
fn split_url(url: &str) -> anyhow::Result<(String, String)> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| anyhow::anyhow!("not an https URL: {url}"))?;
    let (host, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest.to_string(), "/".to_string()),
    };
    Ok((host, path))
}

/// Minimal HTTP/1.1 GET over the shared TLS helper. Reads until the server
/// closes the connection (`Connection: close` is always sent).
struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

async fn https_get(
    host: &str,
    path: &str,
    extra_headers: &[(&str, String)],
) -> anyhow::Result<HttpResponse> {
    let mut tls = update_check::tls_connect(host).await?;
    let mut req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: quicmic\r\nConnection: close\r\n"
    );
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    tls.write_all(req.as_bytes()).await?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match tls.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            // A peer that closes without a TLS close_notify surfaces as
            // UnexpectedEof; treat it as a clean end.
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
    }

    let head_end = find_subslice(&buf, b"\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("response had no header block"))?;
    let head = String::from_utf8_lossy(&buf[..head_end]);
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect();
    Ok(HttpResponse {
        status,
        headers,
        body: buf[head_end + 4..].to_vec(),
    })
}

/// Decode the body: de-chunk when the server used chunked transfer encoding.
fn decode_body(resp: &HttpResponse) -> anyhow::Result<Vec<u8>> {
    let chunked = resp
        .header("transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    if chunked {
        dechunk(&resp.body)
    } else {
        Ok(resp.body.clone())
    }
}

/// Decode HTTP/1.1 chunked transfer encoding.
fn dechunk(mut body: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end =
            find_subslice(body, b"\r\n").ok_or_else(|| anyhow::anyhow!("bad chunk header"))?;
        let size_line = std::str::from_utf8(&body[..line_end])
            .map_err(|_| anyhow::anyhow!("bad chunk header"))?;
        let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| anyhow::anyhow!("bad chunk size"))?;
        body = &body[line_end + 2..];
        if size == 0 {
            break;
        }
        if body.len() < size {
            anyhow::bail!("truncated chunked body");
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size..];
        // Trailing CRLF after each chunk. Fewer than 2 bytes left means the
        // stream ended mid-body — fail loudly rather than returning a silently
        // truncated payload.
        if body.len() < 2 {
            anyhow::bail!("truncated chunked body");
        }
        body = &body[2..];
    }
    Ok(out)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Build the updater batch script. Pure (and unit-tested): it waits for `pid`
/// to disappear, backs up the current exe as `.bak`, moves the downloaded
/// `.new` file over the exe, relaunches the exe with its original arguments,
/// then deletes itself. If the swap fails, the old exe is relaunched instead
/// and a marker file is left in %TEMP% — you're never left with nothing.
/// Windows-only in production (batch staging); compiled for tests everywhere.
#[cfg(any(windows, test))]
pub fn updater_script(pid: u32, exe: &Path, new_exe: &Path, args: &[String]) -> String {
    let quoted_args = args
        .iter()
        .map(|a| format!("\"{}\"", a.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "@echo off\r\n\
         setlocal\r\n\
         set \"PID={pid}\"\r\n\
         set \"NEW={new_exe}\"\r\n\
         set \"EXE={exe}\"\r\n\
         :wait\r\n\
         tasklist /FI \"PID eq %PID%\" 2>nul | find \"%PID%\" >nul\r\n\
         if %errorlevel%==0 ( timeout /t 1 /nobreak >nul & goto wait )\r\n\
         copy /y \"%EXE%\" \"%EXE%.bak\" >nul\r\n\
         move /y \"%NEW%\" \"%EXE%\" >nul\r\n\
         if not %errorlevel%==0 echo %date% %time% update failed: could not replace the exe > \"%TEMP%\\quicmic-update-failed.txt\"\r\n\
         start \"\" \"%EXE%\" {quoted_args}\r\n\
         del \"%~f0\"\r\n",
        new_exe = new_exe.display(),
        exe = exe.display(),
    )
}

/// Write the updater batch to %TEMP% and spawn it detached, so it survives
/// our own exit. The caller should quit right after (graceful shutdown lets
/// the batch's wait loop finish quickly).
#[cfg(windows)]
fn stage_self_update(new_exe: &Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let pid = std::process::id();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let script = updater_script(pid, &exe, new_exe, &args);
    let bat = std::env::temp_dir().join(format!("quicmic-update-{pid}.bat"));
    std::fs::write(&bat, script)?;

    // DETACHED_PROCESS so no console window flashes; the batch relaunches the
    // exe with `start`, which gives it its own window as usual.
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x00000008;
    std::process::Command::new("cmd")
        .args(["/C", &bat.to_string_lossy()])
        .creation_flags(DETACHED_PROCESS)
        .spawn()?;
    Ok(())
}

/// Hand a downloaded `.msi` to Windows Installer (passive UI: progress bar,
/// no prompts beyond a possible UAC consent) and let it replace the install.
/// The caller quits right after, so no QuicMic file is locked when the
/// installer runs. Unlike the exe swap there is no automatic relaunch — the
/// user reopens QuicMic from the Start Menu when the installer finishes.
#[cfg(windows)]
/// Record that an MSI update to `tag` was staged, so the next launch can
/// verify it actually landed (see `check_pending_update` in main.rs).
/// Best-effort: a failure to write the marker must never fail the update.
fn write_pending_marker(data_dir: &Path, tag: &str) {
    let target = match update_check::parse_version(tag) {
        Some((maj, min, patch)) => format!("{maj}.{min}.{patch}"),
        None => return,
    };
    let body = serde_json::json!({
        "target": target,
        "tag": tag,
        "url": format!("https://github.com/{REPO}/releases/tag/{tag}"),
    });
    let path = data_dir.join("update-pending.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, serde_json::to_string(&body).unwrap_or_default());
}

#[cfg(windows)]
fn stage_msi_install(msi: &Path) -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x00000008;
    std::process::Command::new("msiexec")
        .args(["/i"])
        .arg(msi)
        .args(["/passive", "/norestart"])
        .creation_flags(DETACHED_PROCESS)
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn updater_script_waits_swaps_and_restarts() {
        let script = updater_script(
            1234,
            Path::new("C:\\apps\\quicmic.exe"),
            Path::new("C:\\apps\\quicmic.exe.new"),
            &["--theme".into(), "neon".into()],
        );
        assert!(script.contains("set \"PID=1234\""));
        // Waits for our PID to exit…
        assert!(script.contains("tasklist /FI \"PID eq %PID%\""));
        // …backs up the old exe, swaps the new one over it…
        assert!(script.contains("copy /y \"%EXE%\" \"%EXE%.bak\""));
        assert!(script.contains("move /y \"%NEW%\" \"%EXE%\""));
        // …relaunches with the original args, and cleans itself up.
        assert!(script.contains("start \"\" \"%EXE%\" \"--theme\" \"neon\""));
        assert!(script.contains("del \"%~f0\""));
    }

    #[test]
    fn updater_script_quotes_paths_with_spaces() {
        let script = updater_script(
            7,
            Path::new("C:\\my apps\\quicmic.exe"),
            Path::new("C:\\my apps\\quicmic.exe.new"),
            &[],
        );
        assert!(script.contains("set \"EXE=C:\\my apps\\quicmic.exe\""));
    }

    #[test]
    fn token_from_file_reads_first_line() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "  test_token_fixture  ").unwrap();
        writeln!(f, "second line").unwrap();
        assert_eq!(
            token_from_file(f.path()),
            Some("test_token_fixture".to_string())
        );
    }

    #[test]
    fn token_from_file_rejects_missing_and_blank() {
        assert_eq!(token_from_file(Path::new("/nope/missing.txt")), None);
        let f = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(token_from_file(f.path()), None);
    }

    #[test]
    fn split_url_splits_host_and_path() {
        let (h, p) = split_url("https://api.github.com/repos/a/b/releases/latest").unwrap();
        assert_eq!(h, "api.github.com");
        assert_eq!(p, "/repos/a/b/releases/latest");
        assert!(split_url("http://insecure/x").is_err());
    }

    #[test]
    fn pick_install_asset_prefers_setup_exe_for_arch() {
        let assets = serde_json::json!([
            {"name": "quicmic-x86_64-pc-windows-msvc.zip", "url": "https://api.github.com/z"},
            {"name": "quicmic-x86_64-pc-windows-msvc-setup.exe", "url": "https://api.github.com/s"},
            {"name": "quicmic-aarch64-pc-windows-msvc-setup.exe", "url": "https://api.github.com/sa"},
        ]);
        let picked = pick_install_asset(assets.as_array().unwrap());
        if std::env::consts::ARCH == "x86_64" {
            assert_eq!(
                picked,
                Some((
                    "quicmic-x86_64-pc-windows-msvc-setup.exe".to_string(),
                    "https://api.github.com/s".to_string()
                ))
            );
        } else {
            // On other archs the x86_64 setup exe must never win silently.
            assert_ne!(
                picked.as_ref().map(|(n, _)| n.as_str()),
                Some("quicmic-x86_64-pc-windows-msvc-setup.exe")
            );
        }
    }

    #[test]
    fn pick_install_asset_selects_bare_exe() {
        let assets = serde_json::json!([
            {"name": "quicmic-v0.4.1-windows.zip", "url": "https://api.github.com/z"},
            {"name": "quicmic.exe", "url": "https://api.github.com/a"},
        ]);
        assert_eq!(
            pick_install_asset(assets.as_array().unwrap()),
            Some((
                "quicmic.exe".to_string(),
                "https://api.github.com/a".to_string()
            ))
        );
    }

    #[test]
    fn pick_install_asset_none_when_no_installable() {
        // Zip-only release: report clearly, never download the zip as the exe.
        let assets = serde_json::json!([
            {"name": "quicmic-v0.4.1-windows.zip", "url": "https://api.github.com/z"},
            {"name": "notes.txt", "url": "https://api.github.com/t"},
        ]);
        assert_eq!(pick_install_asset(assets.as_array().unwrap()), None);
        assert_eq!(pick_install_asset(&[]), None);
    }

    #[test]
    fn pick_install_asset_rejects_exe_suffixed_archives() {
        let assets = serde_json::json!([
            {"name": "quicmic.exe.zip", "url": "https://api.github.com/z"},
        ]);
        assert_eq!(pick_install_asset(assets.as_array().unwrap()), None);
    }

    #[test]
    fn looks_like_pe_checks_mz_magic() {
        assert!(looks_like_pe(b"MZ\x90\x00rest of a pe"));
        assert!(!looks_like_pe(b""));
        assert!(!looks_like_pe(b"M"));
        // An HTML error page served with a 200 must not pass as an exe.
        assert!(!looks_like_pe(b"<html>not found</html>"));
    }

    #[test]
    fn looks_like_msi_checks_ole2_magic() {
        assert!(looks_like_msi(&[
            0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0x00
        ]));
        assert!(!looks_like_msi(b"MZ\x90\x00rest of a pe"));
        assert!(!looks_like_msi(b""));
    }

    #[test]
    fn pick_install_asset_prefers_msi_for_arch() {
        let assets = serde_json::json!([
            {"name": "quicmic-x86_64-pc-windows-msvc.zip", "url": "https://api.github.com/z"},
            {"name": "quicmic-x86_64-pc-windows-msvc.msi", "url": "https://api.github.com/m"},
            {"name": "quicmic-aarch64-pc-windows-msvc.msi", "url": "https://api.github.com/ma"},
        ]);
        let picked = pick_install_asset(assets.as_array().unwrap());
        if std::env::consts::ARCH == "x86_64" {
            assert_eq!(
                picked,
                Some((
                    "quicmic-x86_64-pc-windows-msvc.msi".to_string(),
                    "https://api.github.com/m".to_string()
                ))
            );
        } else {
            // On other archs the x86_64 .msi must never win silently.
            assert_ne!(
                picked.as_ref().map(|(n, _)| n.as_str()),
                Some("quicmic-x86_64-pc-windows-msvc.msi")
            );
        }
    }

    #[test]
    fn dechunk_rejects_truncated_body() {
        // Stream ends mid-body: chunk data present but the trailing CRLF (and
        // terminator) never arrive — must fail, not return a partial payload.
        assert!(dechunk(b"6\r\nworld!\r").is_err());
        assert!(dechunk(b"6\r\nworld!").is_err());
    }

    #[test]
    fn dechunk_decodes_chunks() {
        // "Hello, " (7) + "world!" (6), then the zero terminator.
        let raw = b"7\r\nHello, \r\n6\r\nworld!\r\n0\r\n\r\n";
        assert_eq!(dechunk(raw).unwrap(), b"Hello, world!");
    }

    #[test]
    fn decode_body_passes_through_with_content_length() {
        let resp = HttpResponse {
            status: 200,
            headers: vec![("content-length".into(), "3".into())],
            body: b"abc".to_vec(),
        };
        assert_eq!(decode_body(&resp).unwrap(), b"abc");
    }
}
