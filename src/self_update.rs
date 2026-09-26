//! Self-updater for the exe: the `update` console command.
//!
//! Flow: read a GitHub token (`QUICMIC_GITHUB_TOKEN` env var, or
//! `github_token.txt` next to the exe) → ask the GitHub API for the private
//! repo's latest release → compare against the running version → download the
//! `.exe` asset next to the running exe → download the release's published
//! `<exe>.sha256` checksum asset → verify the exe's SHA-256 against it →
//! write a small updater batch file → spawn it detached → the console quits
//! gracefully; the batch waits for our PID to exit, swaps the new exe over
//! the old one, restarts it with the same arguments, and deletes itself.
//!
//! Integrity is fail-closed: a missing/unparsable checksum asset or a digest
//! mismatch aborts the update with a clear error — the running exe is never
//! replaced by bytes we haven't verified. The old PE magic-byte heuristic
//! remains as a defense-in-depth sanity check, not as the gate.
//!
//! No HTTP-client crate is pulled in: like [`crate::update_check`], this
//! reuses the crate's existing TLS stack (`rustls` + `ring` + the OS trust
//! store) and speaks raw HTTP/1.1, including redirect-following and
//! chunked-body decoding for the API responses.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ring::digest::{digest, SHA256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::update_check;

/// `owner/repo` whose releases carry the exe. Private, so the API calls below
/// need the token — the unauthenticated startup check in
/// [`crate::update_check`] targets the same repo but can only stay silent
/// without auth; it never reports a version on its own.
const PRIVATE_REPO: &str = "crtGhoul/quicmic-zero-ui";

/// Overall budgets so a wedged network can never hang the console forever.
const API_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// Where the token lives if not given via env.
const TOKEN_FILE: &str = "github_token.txt";

/// A release worth installing: its tag plus the `.exe` asset's API URL and
/// the API URL of its published `<exe>.sha256` checksum asset. The checksum
/// URL is required, not optional — a release without one is refused outright
/// (fail closed) rather than installed on magic bytes alone.
pub struct ReleaseInfo {
    pub tag: String,
    pub asset_name: String,
    pub asset_api_url: String,
    pub checksum_api_url: String,
}

/// GitHub token for the private repo: env var wins, then `github_token.txt`
/// next to the exe (first line). The value is never logged.
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
pub async fn run_update() -> anyhow::Result<bool> {
    // Platform gate first: self-update only exists for the Windows exe, so on
    // other platforms bail before any network work instead of downloading an
    // exe we would only discard. (`cfg!` rather than `#[cfg]` so the rest of
    // the function still compiles — and stays dead-code-warning-free — on
    // every platform.)
    if !cfg!(windows) {
        println!("Self-update is only wired up for the Windows exe.");
        return Ok(false);
    }

    let token = github_token().ok_or_else(|| {
        anyhow::anyhow!(
            "No GitHub token found.\n\
             Create a fine-grained personal access token (read-only, just the \
             quicmic-zero-ui repo):\n  \
             https://github.com/settings/personal-access-tokens/new\n\
             Then either set the QUICMIC_GITHUB_TOKEN environment variable, or \
             save the token as the first line of {TOKEN_FILE} next to the exe."
        )
    })?;

    println!("Checking for updates…");
    let rel = tokio::time::timeout(API_TIMEOUT, latest_release(&token))
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
    let dest = exe.with_extension("exe.new");
    let bytes = tokio::time::timeout(DOWNLOAD_TIMEOUT, download_asset(&rel.asset_api_url, &token))
        .await
        .map_err(|_| anyhow::anyhow!("download timed out"))??;
    println!("Downloaded {:.1} MB.", bytes.len() as f64 / 1_048_576.0);

    // Integrity gate: fetch the release's published checksum file and verify
    // the exe against it *before* anything is written to disk. Any failure
    // here — missing asset, unparsable file, digest mismatch — aborts the
    // update; the running exe is never touched.
    let sum_bytes = tokio::time::timeout(
        DOWNLOAD_TIMEOUT,
        download_asset(&rel.checksum_api_url, &token),
    )
    .await
    .map_err(|_| anyhow::anyhow!("checksum download timed out"))??;
    let sum_text = std::str::from_utf8(&sum_bytes).map_err(|_| {
        anyhow::anyhow!("checksum file is not valid UTF-8 — not staging the update")
    })?;
    let expected = select_digest(&parse_checksum_file(sum_text)?, &rel.asset_name)?;
    if !verify_sha256(&bytes, &expected) {
        anyhow::bail!(
            "SHA-256 mismatch for {} — the download is corrupt or tampered with; not staging it",
            rel.asset_name
        );
    }
    println!("SHA-256 verified.");

    // Defense in depth: the checksum is the gate, but keep the old magic-byte
    // sanity check too — a verified exe that isn't a PE would be bizarre and
    // worth refusing loudly.
    if !looks_like_pe(&bytes) {
        anyhow::bail!(
            "downloaded asset passed its checksum but is not a Windows executable ({} bytes, no MZ header) — not staging it",
            bytes.len()
        );
    }
    std::fs::write(&dest, &bytes)?;

    #[cfg(windows)]
    {
        stage_self_update(&dest)?;
        println!("Update staged — restarting into the new version…");
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

/// Query the private repo's latest release and pick its `.exe` asset.
async fn latest_release(token: &str) -> anyhow::Result<ReleaseInfo> {
    let resp = https_get(
        "api.github.com",
        &format!("/repos/{PRIVATE_REPO}/releases/latest"),
        &[
            ("Authorization", format!("Bearer {token}")),
            ("Accept", "application/vnd.github+json".to_string()),
            ("X-GitHub-Api-Version", "2022-11-28".to_string()),
        ],
    )
    .await?;
    match resp.status {
        200 => {}
        401 => anyhow::bail!("GitHub rejected the token (401) — check it hasn't expired"),
        404 => anyhow::bail!("release not found (404) — token needs read access to {PRIVATE_REPO}"),
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
        .and_then(|assets| pick_exe_asset(assets))
        .ok_or_else(|| anyhow::anyhow!("latest release has no .exe asset"))?;
    // The checksum asset is mandatory: without it we cannot verify the exe,
    // so the update is refused here rather than halfway through the download.
    // Release process note: whenever the bare exe is attached to a release by
    // hand, its `<exe>.sha256` (e.g. `sha256sum quicmic.exe > quicmic.exe.sha256`)
    // must be attached next to it, or the updater will decline to install it.
    let checksum_name = format!("{asset_name}.sha256");
    let checksum_api_url = v
        .get("assets")
        .and_then(|a| a.as_array())
        .and_then(|assets| {
            assets.iter().find_map(|a| {
                if a.get("name")?.as_str()? != checksum_name {
                    return None;
                }
                Some(a.get("url")?.as_str()?.to_string())
            })
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "latest release has no {checksum_name} checksum asset — refusing to self-update"
            )
        })?;
    Ok(ReleaseInfo {
        tag,
        asset_name,
        asset_api_url,
        checksum_api_url,
    })
}

/// Pick the release asset to install: the first whose name ends exactly in
/// `.exe`. Pure, so it is unit-testable. Returns `(name, api_url)`.
///
/// A release with no bare `.exe` asset (only a `.zip`, for example) yields
/// `None` — the caller reports that clearly instead of downloading the wrong
/// file. A name like `quicmic.exe.zip` does not qualify.
fn pick_exe_asset(assets: &[serde_json::Value]) -> Option<(String, String)> {
    assets.iter().find_map(|a| {
        let name = a.get("name")?.as_str()?;
        if !name.ends_with(".exe") {
            return None;
        }
        let url = a.get("url")?.as_str()?.to_string();
        Some((name.to_string(), url))
    })
}

/// Cheap integrity gate for a downloaded Windows executable: every PE starts
/// with the `MZ` magic. Catches truncated downloads and mis-served error pages
/// (an HTML 404 body saved as the "exe") before they can be staged.
fn looks_like_pe(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == b'M' && bytes[1] == b'Z'
}

/// SHA-256 of `bytes` as lowercase hex, via the crate's pinned `ring`
/// provider (no new crypto dependency). Pure, so it is unit-testable.
fn sha256_hex(bytes: &[u8]) -> String {
    let d = digest(&SHA256, bytes);
    let mut out = String::with_capacity(64);
    for b in d.as_ref() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Compare `bytes` against an expected hex digest (case-insensitive). Pure.
/// The digest is public data, so a plain comparison is fine — there is no
/// secret here to protect with constant-time compare.
fn verify_sha256(bytes: &[u8], expected_hex: &str) -> bool {
    sha256_hex(bytes) == expected_hex.to_ascii_lowercase()
}

/// One parsed line of a `.sha256` checksum file.
struct ChecksumEntry {
    digest: String,
    file_name: Option<String>,
}

/// Parse a `.sha256` checksum file into entries. Accepts the formats
/// `cargo-dist` / `sha256sum` produce:
///   `<hex>`                    (bare digest)
///   `<hex>  <file>`            (coreutils text/binary mode)
///   `SHA256 (<file>) = <hex>`  (BSD tag form)
/// `#` comments and blank lines are ignored. An unrecognized line is an
/// error, not a skip — a checksum file we cannot fully parse must never be
/// treated as "no checksum". Pure, so it is unit-testable.
fn parse_checksum_file(text: &str) -> anyhow::Result<Vec<ChecksumEntry>> {
    let mut entries = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        entries.push(parse_checksum_line(line).ok_or_else(|| {
            anyhow::anyhow!(
                "checksum file line {} is not a recognized format: {line:?}",
                i + 1
            )
        })?);
    }
    if entries.is_empty() {
        anyhow::bail!("checksum file has no usable entries");
    }
    Ok(entries)
}

fn parse_checksum_line(line: &str) -> Option<ChecksumEntry> {
    // BSD tag form: SHA256 (name) = <hex>
    if let Some(rest) = line.strip_prefix("SHA256 (") {
        let (name, rest) = rest.split_once(") =")?;
        return valid_digest(rest.trim()).map(|digest| ChecksumEntry {
            digest,
            file_name: Some(name.trim().to_string()),
        });
    }
    // coreutils: `<hex>[* ]name`, or a bare `<hex>`.
    let mut parts = line.split_whitespace();
    let digest = valid_digest(parts.next()?)?;
    let file_name = parts.next().map(|n| n.trim_start_matches('*').to_string());
    if parts.next().is_some() {
        return None; // trailing junk — not a format we understand
    }
    Some(ChecksumEntry { digest, file_name })
}

/// A digest is exactly 64 hex chars; normalized to lowercase.
fn valid_digest(s: &str) -> Option<String> {
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

/// Pick the expected digest for `asset_name` from the parsed entries: prefer
/// an entry that names the asset (compared on the last path component, so
/// `subdir/quicmic.exe` still matches), otherwise accept a single bare entry.
/// Anything ambiguous fails closed. Pure, so it is unit-testable.
fn select_digest(entries: &[ChecksumEntry], asset_name: &str) -> anyhow::Result<String> {
    let base = asset_name.rsplit(['/', '\\']).next().unwrap_or(asset_name);
    if let Some(e) = entries.iter().find(|e| {
        e.file_name
            .as_ref()
            .is_some_and(|n| n.rsplit(['/', '\\']).next().unwrap_or(n.as_str()) == base)
    }) {
        return Ok(e.digest.clone());
    }
    match entries
        .iter()
        .filter(|e| e.file_name.is_none())
        .collect::<Vec<_>>()
        .as_slice()
    {
        [e] => Ok(e.digest.clone()),
        _ => anyhow::bail!(
            "checksum file does not identify {asset_name} unambiguously — refusing to update"
        ),
    }
}

/// Download an asset API URL, following redirects (the API 302s to a signed
/// `objects.githubusercontent.com` URL). The token is only sent to
/// `api.github.com`, never to the redirect target. Returns the raw bytes —
/// the caller decides what integrity checks to run before writing anything
/// to disk. The asset is small enough (~8 MB) to hold in memory.
async fn download_asset(api_url: &str, token: &str) -> anyhow::Result<Vec<u8>> {
    let mut url = api_url.to_string();
    for _ in 0..5 {
        let (host, path) = split_url(&url)?;
        let mut headers = vec![("Accept", "application/octet-stream".to_string())];
        if host == "api.github.com" {
            headers.push(("Authorization", format!("Bearer {token}")));
        }
        let resp = https_get(&host, &path, &headers).await?;
        match resp.status {
            200 => return decode_body(&resp),
            301 | 302 | 303 | 307 | 308 => {
                url = resp
                    .header("location")
                    .ok_or_else(|| anyhow::anyhow!("redirect without Location"))?
                    .to_string();
            }
            401 => anyhow::bail!("GitHub rejected the token (401)"),
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
        writeln!(f, "  ghp_secret123  ").unwrap();
        writeln!(f, "second line").unwrap();
        assert_eq!(token_from_file(f.path()), Some("ghp_secret123".to_string()));
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
    fn pick_exe_asset_selects_first_exe() {
        let assets = serde_json::json!([
            {"name": "quicmic-v0.4.1-windows.zip", "url": "https://api.github.com/z"},
            {"name": "quicmic.exe", "url": "https://api.github.com/a"},
            {"name": "quicmic-arm64.exe", "url": "https://api.github.com/b"},
        ]);
        assert_eq!(
            pick_exe_asset(assets.as_array().unwrap()),
            Some((
                "quicmic.exe".to_string(),
                "https://api.github.com/a".to_string()
            ))
        );
    }

    #[test]
    fn pick_exe_asset_none_when_no_exe() {
        // v0.4.1 shipped only a .zip: the updater must report "no .exe asset"
        // clearly, never download the zip as if it were the exe.
        let assets = serde_json::json!([
            {"name": "quicmic-v0.4.1-windows.zip", "url": "https://api.github.com/z"},
            {"name": "notes.txt", "url": "https://api.github.com/t"},
        ]);
        assert_eq!(pick_exe_asset(assets.as_array().unwrap()), None);
        assert_eq!(pick_exe_asset(&[]), None);
    }

    #[test]
    fn pick_exe_asset_rejects_exe_suffixed_archives() {
        let assets = serde_json::json!([
            {"name": "quicmic.exe.zip", "url": "https://api.github.com/z"},
        ]);
        assert_eq!(pick_exe_asset(assets.as_array().unwrap()), None);
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

    #[test]
    fn sha256_hex_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn verify_sha256_accepts_match_and_rejects_tamper() {
        let good = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verify_sha256(b"abc", good));
        // Case-insensitive on the expected side.
        assert!(verify_sha256(b"abc", &good.to_ascii_uppercase()));
        // One flipped byte fails.
        assert!(!verify_sha256(b"abd", good));
        assert!(!verify_sha256(b"", good));
    }

    #[test]
    fn parse_checksum_file_accepts_known_formats() {
        let text = "# cargo-dist style, coreutils-compatible\n\
                    ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  quicmic.exe\n\
                    \n\
                    e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 *other.bin\n";
        let entries = parse_checksum_file(text).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(entries[0].file_name.as_deref(), Some("quicmic.exe"));
        assert_eq!(entries[1].file_name.as_deref(), Some("other.bin"));

        // Bare digest.
        let bare = "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD\n";
        let entries = parse_checksum_file(bare).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(entries[0].file_name, None);

        // BSD tag form.
        let bsd = "SHA256 (quicmic.exe) = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n";
        let entries = parse_checksum_file(bsd).unwrap();
        assert_eq!(entries[0].file_name.as_deref(), Some("quicmic.exe"));
    }

    #[test]
    fn parse_checksum_file_rejects_garbage() {
        assert!(parse_checksum_file("not a checksum\n").is_err());
        assert!(parse_checksum_file("").is_err());
        assert!(parse_checksum_file("# only a comment\n\n").is_err());
        // Truncated hex.
        assert!(parse_checksum_file("ba7816bf  quicmic.exe\n").is_err());
        // Trailing junk after the filename.
        assert!(parse_checksum_file(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad quicmic.exe extra\n"
        )
        .is_err());
    }

    #[test]
    fn select_digest_prefers_name_match_then_bare() {
        let entries = parse_checksum_file(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  other.exe\n\
             ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  quicmic.exe\n",
        )
        .unwrap();
        assert_eq!(
            select_digest(&entries, "quicmic.exe").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Entry naming a subdirectory still matches on the file name.
        let entries = parse_checksum_file(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  dist/quicmic.exe\n",
        )
        .unwrap();
        assert_eq!(
            select_digest(&entries, "quicmic.exe").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // A single bare digest is accepted when nothing is named.
        let entries = parse_checksum_file(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n",
        )
        .unwrap();
        assert_eq!(
            select_digest(&entries, "quicmic.exe").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn select_digest_fails_closed_when_ambiguous() {
        // Named entries, none of them ours.
        let entries = parse_checksum_file(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  other.exe\n",
        )
        .unwrap();
        assert!(select_digest(&entries, "quicmic.exe").is_err());
        // Two bare digests — can't tell which is ours.
        let entries = parse_checksum_file(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
             bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
        )
        .unwrap();
        assert!(select_digest(&entries, "quicmic.exe").is_err());
    }
}
