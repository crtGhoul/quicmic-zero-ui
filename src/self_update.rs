//! Self-updater for the exe: the `update` console command.
//!
//! Flow: read a GitHub token (`QUICMIC_GITHUB_TOKEN` env var, or
//! `github_token.txt` next to the exe) → ask the GitHub API for the private
//! repo's latest release → compare against the running version → download the
//! `.exe` asset next to the running exe → write a small updater batch file →
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

/// `owner/repo` whose releases carry the exe. Private, so the API calls below
/// need the token — which is also why the plain startup update check (public
/// upstream repo, no auth) can't cover it.
const PRIVATE_REPO: &str = "crtGhoul/quicmic-zero-ui";

/// Overall budgets so a wedged network can never hang the console forever.
const API_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// Where the token lives if not given via env.
const TOKEN_FILE: &str = "github_token.txt";

/// A release worth installing: its tag plus the `.exe` asset's API URL.
pub struct ReleaseInfo {
    pub tag: String,
    pub asset_name: String,
    pub asset_api_url: String,
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
    let bytes = tokio::time::timeout(
        DOWNLOAD_TIMEOUT,
        download_asset(&rel.asset_api_url, &token, &dest),
    )
    .await
    .map_err(|_| anyhow::anyhow!("download timed out"))??;
    println!("Downloaded {:.1} MB.", bytes as f64 / 1_048_576.0);

    #[cfg(windows)]
    {
        stage_self_update(&dest)?;
        println!("Update staged — restarting into the new version…");
        Ok(true)
    }
    #[cfg(not(windows))]
    {
        let _ = dest;
        println!("Self-update is only wired up for the Windows exe.");
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
        .and_then(|assets| {
            assets.iter().find_map(|a| {
                let name = a.get("name")?.as_str()?;
                if !name.ends_with(".exe") {
                    return None;
                }
                let url = a.get("url")?.as_str()?.to_string();
                Some((name.to_string(), url))
            })
        })
        .ok_or_else(|| anyhow::anyhow!("latest release has no .exe asset"))?;
    Ok(ReleaseInfo {
        tag,
        asset_name,
        asset_api_url,
    })
}

/// Download an asset API URL, following redirects (the API 302s to a signed
/// `objects.githubusercontent.com` URL). The token is only sent to
/// `api.github.com`, never to the redirect target. The asset is small enough
/// (~8 MB) to hold in memory, then written to `dest` in one go.
async fn download_asset(api_url: &str, token: &str, dest: &Path) -> anyhow::Result<u64> {
    let mut url = api_url.to_string();
    for _ in 0..5 {
        let (host, path) = split_url(&url)?;
        let mut headers = vec![("Accept", "application/octet-stream".to_string())];
        if host == "api.github.com" {
            headers.push(("Authorization", format!("Bearer {token}")));
        }
        let resp = https_get(&host, &path, &headers).await?;
        match resp.status {
            200 => {
                let body = decode_body(&resp)?;
                std::fs::write(dest, &body)?;
                return Ok(body.len() as u64);
            }
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
        // Trailing CRLF after each chunk.
        if body.len() < 2 {
            break;
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
