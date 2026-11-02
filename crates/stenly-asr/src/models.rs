//! Model catalog + first-run download into the cache dir.
//!
//! Every file is pinned to an exact size and SHA-256, so "installed" means the
//! bytes on disk are the bytes we shipped against, not just that a file with
//! the right name exists. Downloads are:
//! - **atomic**: bytes go to `<name>.part` and are renamed into place only
//!   after the size and checksum match, so a crash never leaves a half-file
//!   that looks installed;
//! - **resumable**: a leftover `.part` is continued with an HTTP range request
//!   (and thrown away if the server or the checksum disagrees);
//! - **serialized**: an advisory lock per directory means two downloaders of
//!   the same file (the app and the CLI, or onboarding and a preload) wait for
//!   each other instead of fighting over one `.part` file.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// One file of a model, pinned by size and checksum.
pub struct ModelFile {
    pub name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

pub struct ModelSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub language: &'static str,
    /// 1–5 dots (and the x/10 rating shown next to them).
    pub speed: f32,
    pub accuracy: f32,
    pub description: &'static str,
    /// Individual files on Hugging Face, pinned to a commit (resumable, no
    /// tar/bzip2 dependency).
    pub base_url: &'static str,
    pub encoder: &'static str,
    pub decoder: &'static str,
    pub joiner: &'static str,
    /// Everything that must be on disk, `tokens.txt` included.
    pub files: &'static [ModelFile],
    pub approx_mb: u32,
}

pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "parakeet-tdt-0.6b-v2-int8",
        name: "Parakeet V2",
        language: "English",
        speed: 9.9,
        accuracy: 9.4,
        description: "NVIDIA's Parakeet V2, optimized for lightning-fast English transcription.",
        base_url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8/resolve/1ab9323565ddb038682214b292f588070a538ce2",
        encoder: "encoder.int8.onnx",
        decoder: "decoder.int8.onnx",
        joiner: "joiner.int8.onnx",
        files: &[
            ModelFile {
                name: "encoder.int8.onnx",
                size: 652_184_296,
                sha256: "a32b12d17bbbc309d0686fbbcc2987b5e9b8333a7da83fa6b089f0a2acd651ab",
            },
            ModelFile {
                name: "decoder.int8.onnx",
                size: 7_257_753,
                sha256: "b6bb64963457237b900e496ee9994b59294526439fbcc1fecf705b31a15c6b4e",
            },
            ModelFile {
                name: "joiner.int8.onnx",
                size: 1_739_080,
                sha256: "7946164367946e7f9f29a122407c3252b680dbae9a51343eb2488d057c3c43d2",
            },
            ModelFile {
                name: "tokens.txt",
                size: 9_384,
                sha256: "ec182b70dd42113aff6c5372c75cac58c952443eb22322f57bbd7f53977d497d",
            },
        ],
        approx_mb: 660,
    },
    ModelSpec {
        id: "parakeet-tdt-0.6b-v3-int8",
        name: "Parakeet V3",
        language: "Multilingual",
        speed: 9.9,
        accuracy: 9.4,
        description: "Parakeet V3 with English and 25 European languages.",
        base_url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/resolve/2bda32ec70b097a55adaa07d9a7173915b43cc78",
        encoder: "encoder.int8.onnx",
        decoder: "decoder.int8.onnx",
        joiner: "joiner.int8.onnx",
        files: &[
            ModelFile {
                name: "encoder.int8.onnx",
                size: 652_184_281,
                sha256: "acfc2b4456377e15d04f0243af540b7fe7c992f8d898d751cf134c3a55fd2247",
            },
            ModelFile {
                name: "decoder.int8.onnx",
                size: 11_845_275,
                sha256: "179e50c43d1a9de79c8a24149a2f9bac6eb5981823f2a2ed88d655b24248db4e",
            },
            ModelFile {
                name: "joiner.int8.onnx",
                size: 6_355_277,
                sha256: "3164c13fc2821009440d20fcb5fdc78bff28b4db2f8d0f0b329101719c0948b3",
            },
            ModelFile {
                name: "tokens.txt",
                size: 93_939,
                sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
            },
        ],
        approx_mb: 670,
    },
];

/// Silero VAD v5. v4 (`silero_vad.onnx`) badly under-detects speech that has
/// been through a call codec (Zoom/Meet far-end audio): on a real 98-min call
/// it marked <5% of the remote side as speech, so almost none of it was
/// transcribed. v5 scores the same audio ~0.8.
const VAD_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad_v5.onnx";
const VAD_FILE: &str = "silero_vad_v5.onnx";
const VAD_SIZE: u64 = 2_313_101;
const VAD_SHA256: &str = "6b99cbfd39246b6706f98ec13c7c50c6b299181f2474fa05cbc8046acc274396";

pub fn find(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.id == id)
}

/// Directory name used by the upstream sherpa-onnx release tarballs, so a
/// manually extracted archive is picked up too.
fn dir_for(spec: &ModelSpec, root: &Path) -> PathBuf {
    root.join(format!("sherpa-onnx-nemo-{}", spec.id))
}

/// A complete file: right name, right size. (The checksum was verified before
/// it was renamed into place; re-hashing 650 MB on every UI refresh is not an
/// option, but a truncated or swapped file is still caught by its size.)
fn file_ok(path: &Path, size: u64) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == size)
}

fn files_ok(dir: &Path, files: &[ModelFile]) -> bool {
    files.iter().all(|f| file_ok(&dir.join(f.name), f.size))
}

/// The voice-activity model every transcription also needs.
pub fn vad_installed(root: &Path) -> bool {
    file_ok(&root.join(VAD_FILE), VAD_SIZE)
}

/// Every file is there at its full size (cheap: only looks at the folder). It
/// may still be unchecked or damaged; see [`is_installed`].
pub fn is_present(spec: &ModelSpec, root: &Path) -> bool {
    files_ok(&dir_for(spec, root), spec.files) && vad_installed(root)
}

/// Everything transcription needs is on disk, complete **and verified**: the
/// speech model's files and the VAD, each with a record that its bytes matched
/// the pinned checksum. This is what "Ready" means in the UI. A folder that is
/// merely present (an older install, a manual extract) becomes installed after
/// [`verify_installed`] has hashed it once.
pub fn is_installed(spec: &ModelSpec, root: &Path) -> bool {
    let dir = dir_for(spec, root);
    let checked =
        |p: &Path| stamp(p).is_some() && std::fs::read_to_string(sidecar(p)).ok() == stamp(p);
    is_present(spec, root)
        && spec.files.iter().all(|f| checked(&dir.join(f.name)))
        && checked(&root.join(VAD_FILE))
}

/// `<name>.ok` records that the file at its current size and mtime matched its
/// checksum, so a launch re-hashes a 650 MB model once, not every time.
fn sidecar(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".ok");
    dest.with_file_name(name)
}

fn stamp(dest: &Path) -> Option<String> {
    let m = std::fs::metadata(dest).ok()?;
    let t = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(format!("{}:{}", m.len(), t.as_nanos()))
}

/// The file is complete *and* its bytes match the pinned checksum (hashed
/// once; remembered in the sidecar until the file changes).
fn verified(dest: &Path, size: u64, sha256: &str) -> bool {
    if !file_ok(dest, size) {
        return false;
    }
    let now = stamp(dest);
    if now.is_some() && std::fs::read_to_string(sidecar(dest)).ok() == now {
        return true;
    }
    if sha256_hex(dest).is_ok_and(|h| h == sha256) {
        if let Some(now) = now {
            let _ = std::fs::write(sidecar(dest), now);
        }
        return true;
    }
    false
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

/// Advisory lock on a directory's downloads, held while a file is fetched.
/// Locks the same file from any thread or process (flock), so the second
/// downloader of a file waits, then finds it complete.
struct DirLock(Option<std::fs::File>);
impl DirLock {
    fn acquire(dir: &Path) -> Self {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(".download.lock"));
        match f {
            Ok(f) => match f.lock() {
                Ok(()) => Self(Some(f)),
                Err(e) => {
                    tracing::warn!(error = %e, "couldn't lock the models folder; downloading without it");
                    Self(None)
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "couldn't create a download lock; downloading without it");
                Self(None)
            }
        }
    }
}
impl Drop for DirLock {
    fn drop(&mut self) {
        if let Some(f) = &self.0 {
            let _ = f.unlock();
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(30)))
        .timeout_recv_response(Some(std::time::Duration::from_secs(60)))
        .build()
        .into()
}

/// Why one attempt to fill the `.part` file stopped.
enum Attempt {
    /// The partial file can't be continued (range refused, bytes don't check
    /// out): drop it and start over, once.
    Restart(anyhow::Error),
    Fail(anyhow::Error),
}

/// Append the remainder of `url` to `part` (or all of it, if the server won't
/// resume). `on_bytes(done)` reports bytes of the whole file present so far.
fn fill_part(
    url: &str,
    part: &Path,
    size: u64,
    on_bytes: &mut dyn FnMut(u64),
) -> std::result::Result<(), Attempt> {
    let mut have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if have > size {
        return Err(Attempt::Restart(anyhow::anyhow!(
            "leftover partial download is larger than the file"
        )));
    }
    if have == size {
        on_bytes(size);
        return Ok(());
    }
    tracing::info!(%url, resume_from = have, "downloading");
    let mut req = agent().get(url);
    if have > 0 {
        req = req.header("Range", format!("bytes={have}-"));
    }
    let resp = match req.call() {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(416)) if have > 0 => {
            return Err(Attempt::Restart(anyhow::anyhow!(
                "server refused to resume"
            )));
        }
        Err(e) => {
            return Err(Attempt::Fail(
                anyhow::Error::new(e).context(format!("GET {url}")),
            ));
        }
    };
    let status = resp.status().as_u16();
    let len: Option<u64> = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let mut file = match status {
        206 => {
            // `Content-Range: bytes <start>-<end>/<total>`: must continue exactly where we stopped.
            let start = resp
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("bytes "))
                .and_then(|v| v.split('-').next())
                .and_then(|v| v.parse::<u64>().ok());
            if start != Some(have) {
                return Err(Attempt::Restart(anyhow::anyhow!(
                    "server resumed from the wrong offset"
                )));
            }
            std::fs::OpenOptions::new()
                .append(true)
                .open(part)
                .map_err(|e| Attempt::Fail(e.into()))?
        }
        _ => {
            // 200: the whole file again (a range-less server, or a fresh start).
            have = 0;
            std::fs::File::create(part).map_err(|e| {
                Attempt::Fail(anyhow::Error::new(e).context(format!("creating {}", part.display())))
            })?
        }
    };
    if let Some(n) = len
        && have + n != size
    {
        return Err(Attempt::Fail(anyhow::anyhow!(
            "{url} is {} bytes, expected {size}",
            have + n
        )));
    }
    let mut reader = resp.into_body().into_reader();
    let mut buf = vec![0u8; 1 << 20];
    let mut done = have;
    on_bytes(done);
    loop {
        let n = reader.read(&mut buf).map_err(|e| {
            Attempt::Fail(anyhow::Error::new(e).context(format!(
                "connection lost at {} MB of {}; Retry continues from here",
                done >> 20,
                size >> 20
            )))
        })?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| Attempt::Fail(e.into()))?;
        done += n as u64;
        on_bytes(done);
    }
    file.sync_all().map_err(|e| Attempt::Fail(e.into()))?;
    if done != size {
        return Err(Attempt::Fail(anyhow::anyhow!(
            "short download for {url}: {done}/{size} bytes; Retry continues from here"
        )));
    }
    Ok(())
}

fn sha256_hex(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(0))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Fetch `url` to `dest`, verified against `size` and `sha256`. Creates the
/// folder, waits for other downloaders of the folder, skips work when `dest`
/// is already complete, resumes a leftover `.part`, and only then renames it
/// into place. Safe to call again after any failure.
pub fn download_verified(
    url: &str,
    dest: &Path,
    size: u64,
    sha256: &str,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<()> {
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let _lock = DirLock::acquire(parent);
    if verified(dest, size, sha256) {
        on_bytes(size);
        return Ok(());
    }
    if dest.exists() {
        // Right name, wrong bytes (an older build raced two downloads into one
        // file): never keep it, fetch a clean copy.
        tracing::warn!(path = %dest.display(), "installed file is damaged; downloading it again");
        let _ = std::fs::remove_file(dest);
        let _ = std::fs::remove_file(sidecar(dest));
    }
    let part = part_path(dest);
    for attempt in 0..2 {
        let resumed = std::fs::metadata(&part).is_ok_and(|m| m.len() > 0);
        let restartable = attempt == 0 && resumed;
        match fill_part(url, &part, size, on_bytes) {
            Ok(()) => {}
            Err(Attempt::Restart(why)) if restartable => {
                tracing::warn!(error = %format!("{why:#}"), %url, "restarting download from scratch");
                let _ = std::fs::remove_file(&part);
                continue;
            }
            Err(Attempt::Restart(why) | Attempt::Fail(why)) => return Err(why),
        }
        if sha256_hex(&part)? == sha256 {
            std::fs::rename(&part, dest)
                .with_context(|| format!("moving {} into place", dest.display()))?;
            if let Some(now) = stamp(dest) {
                let _ = std::fs::write(sidecar(dest), now);
            }
            return Ok(());
        }
        // Never keep bytes that failed the checksum: the next try starts clean.
        let _ = std::fs::remove_file(&part);
        if restartable {
            tracing::warn!(%url, "resumed download failed its checksum; restarting from scratch");
            continue;
        }
        bail!("{url} downloaded but failed its checksum; try again");
    }
    bail!("couldn't download {url}")
}

/// Make sure every file of a model folder is present and verified. `progress`
/// is the fraction of all bytes, so the big encoder doesn't sit at 25%.
fn ensure_files(
    dir: &Path,
    base_url: &str,
    files: &[ModelFile],
    progress: &mut dyn FnMut(f32),
) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let total: u64 = files.iter().map(|f| f.size).sum();
    let mut finished: u64 = 0;
    for f in files {
        download_verified(
            &format!("{base_url}/{}", f.name),
            &dir.join(f.name),
            f.size,
            f.sha256,
            &mut |b| progress(((finished + b) as f32 / total.max(1) as f32).min(1.0)),
        )?;
        finished += f.size;
    }
    Ok(())
}

/// Make sure the model and the VAD are on disk, downloading what's missing or
/// damaged. Returns the model dir.
pub fn ensure(spec: &ModelSpec, root: &Path, on_progress: &mut dyn FnMut(f32)) -> Result<PathBuf> {
    let dir = dir_for(spec, root);
    if is_installed(spec, root) {
        return Ok(dir);
    }
    ensure_files(&dir, spec.base_url, spec.files, on_progress)?;
    ensure_vad(root)?;
    on_progress(1.0);
    Ok(dir)
}

/// Check the bytes of everything [`is_present`] found by size, and record that
/// they passed so [`is_installed`] turns true. Anything that
/// fails its checksum is deleted (so the model reads as not installed and
/// Retry fetches a clean copy) and reported. Slow the first time (hashes
/// ~650 MB): call off the UI thread.
pub fn verify_installed(spec: &ModelSpec, root: &Path) -> Result<()> {
    let dir = dir_for(spec, root);
    let mut bad: Vec<&str> = Vec::new();
    for f in spec.files {
        let p = dir.join(f.name);
        if file_ok(&p, f.size) && !verified(&p, f.size, f.sha256) {
            let _ = std::fs::remove_file(&p);
            let _ = std::fs::remove_file(sidecar(&p));
            bad.push(f.name);
        }
    }
    let vad = root.join(VAD_FILE);
    if file_ok(&vad, VAD_SIZE) && !verified(&vad, VAD_SIZE, VAD_SHA256) {
        let _ = std::fs::remove_file(&vad);
        let _ = std::fs::remove_file(sidecar(&vad));
        bad.push(VAD_FILE);
    }
    if !bad.is_empty() {
        bail!(
            "{} on disk {} damaged and removed. Download again to fix it.",
            bad.join(", "),
            if bad.len() == 1 { "was" } else { "were" }
        );
    }
    Ok(())
}

pub fn remove(spec: &ModelSpec, root: &Path) -> Result<()> {
    let d = dir_for(spec, root);
    if d.exists() {
        std::fs::remove_dir_all(d)?;
    }
    Ok(())
}

pub fn ensure_vad(root: &Path) -> Result<PathBuf> {
    // v4 model from older versions; no longer used
    let _ = std::fs::remove_file(root.join("silero_vad.onnx"));
    let p = root.join(VAD_FILE);
    download_verified(VAD_URL, &p, VAD_SIZE, VAD_SHA256, &mut |_| {})?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        /// Honours `Range`.
        Ranges,
        /// Ignores `Range` and always sends the whole body with 200.
        NoRanges,
        /// Sends only half the body on the first request, then behaves like `Ranges`.
        DropFirst,
        /// 404.
        Missing,
    }

    struct Server {
        url: String,
        hits: Arc<AtomicUsize>,
        ranges: Arc<std::sync::Mutex<Vec<String>>>,
    }

    fn body(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }
    fn sha(b: &[u8]) -> String {
        Sha256::digest(b)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect()
    }

    fn serve(data: Vec<u8>, mode: Mode) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/f",
            listener.local_addr().unwrap().port()
        );
        let hits = Arc::new(AtomicUsize::new(0));
        let ranges = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (h, r) = (hits.clone(), ranges.clone());
        std::thread::spawn(move || {
            for s in listener.incoming() {
                let Ok(mut s) = s else { return };
                let (data, h, r) = (data.clone(), h.clone(), r.clone());
                std::thread::spawn(move || {
                    let n = h.fetch_add(1, Ordering::SeqCst);
                    let mut req = [0u8; 4096];
                    let got = s.read(&mut req).unwrap_or(0);
                    let text = String::from_utf8_lossy(&req[..got]).to_string();
                    let range = text
                        .lines()
                        .find_map(|l| {
                            l.strip_prefix("range: ")
                                .or_else(|| l.strip_prefix("Range: "))
                        })
                        .map(str::to_string);
                    if let Some(range) = &range {
                        r.lock().unwrap().push(range.clone());
                    }
                    if mode == Mode::Missing {
                        let _ = write!(
                            s,
                            "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        );
                        return;
                    }
                    let start = match (&range, mode) {
                        (Some(r), Mode::Ranges | Mode::DropFirst) => r
                            .trim_start_matches("bytes=")
                            .trim_end_matches('-')
                            .parse::<usize>()
                            .unwrap_or(0),
                        _ => 0,
                    };
                    let rest = &data[start..];
                    if start > 0 {
                        let _ = write!(
                            s,
                            "HTTP/1.1 206 Partial Content\r\ncontent-range: bytes {start}-{}/{}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            data.len() - 1,
                            data.len(),
                            rest.len()
                        );
                    } else {
                        let _ = write!(
                            s,
                            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            rest.len()
                        );
                    }
                    let send = if mode == Mode::DropFirst && n == 0 {
                        &rest[..rest.len() / 2]
                    } else {
                        rest
                    };
                    for chunk in send.chunks(16 << 10) {
                        if s.write_all(chunk).is_err() {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                });
            }
        });
        Server { url, hits, ranges }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("stenly-models-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn fresh_profile_creates_missing_folders() {
        let data = body(300_000);
        let srv = serve(data.clone(), Mode::Ranges);
        // Neither the models folder nor the model folder exists yet.
        let root = tmp("fresh");
        let dest = root.join("models/sherpa-onnx-nemo-x/encoder.onnx");
        download_verified(&srv.url, &dest, data.len() as u64, &sha(&data), &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert!(!part_path(&dest).exists(), "no .part left behind");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn two_downloaders_of_one_file_both_succeed() {
        // Regression: onboarding's Download and the dictation preload both fetched
        // the same file into one shared `.part`; the loser's rename failed with
        // "No such file or directory (os error 2)".
        let data = body(2_000_000);
        let srv = serve(data.clone(), Mode::NoRanges);
        let root = tmp("race");
        let dest = root.join("m/encoder.onnx");
        let (size, digest) = (data.len() as u64, sha(&data));
        let spawn = |url: String, dest: PathBuf, digest: String| {
            std::thread::spawn(move || download_verified(&url, &dest, size, &digest, &mut |_| {}))
        };
        let a = spawn(srv.url.clone(), dest.clone(), digest.clone());
        std::thread::sleep(std::time::Duration::from_millis(40));
        let b = spawn(srv.url.clone(), dest.clone(), digest);
        a.join().unwrap().unwrap();
        b.join().unwrap().unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(
            srv.hits.load(Ordering::SeqCst),
            1,
            "the second downloader found the file complete and didn't fetch again"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn already_complete_file_is_not_fetched() {
        let data = body(10_000);
        let srv = serve(data.clone(), Mode::Ranges);
        let root = tmp("done");
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("f");
        std::fs::write(&dest, &data).unwrap();
        download_verified(&srv.url, &dest, data.len() as u64, &sha(&data), &mut |_| {}).unwrap();
        assert_eq!(srv.hits.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn leftover_part_is_resumed_with_a_range_request() {
        let data = body(500_000);
        let srv = serve(data.clone(), Mode::Ranges);
        let root = tmp("resume");
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("f");
        std::fs::write(part_path(&dest), &data[..123_456]).unwrap();
        let mut last = 0;
        download_verified(&srv.url, &dest, data.len() as u64, &sha(&data), &mut |b| {
            last = b
        })
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(srv.ranges.lock().unwrap().as_slice(), ["bytes=123456-"]);
        assert_eq!(last, data.len() as u64, "progress reaches 100%");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_download_keeps_the_part_and_retry_finishes_it() {
        let data = body(400_000);
        let srv = serve(data.clone(), Mode::DropFirst);
        let root = tmp("retry");
        let dest = root.join("f");
        let (size, digest) = (data.len() as u64, sha(&data));
        let err = download_verified(&srv.url, &dest, size, &digest, &mut |_| {}).unwrap_err();
        assert!(format!("{err:#}").contains("Retry"), "{err:#}");
        assert!(
            !dest.exists(),
            "a half-downloaded file is never put in place"
        );
        let kept = std::fs::metadata(part_path(&dest)).unwrap().len();
        assert!(
            kept > 0 && kept < size,
            "partial bytes kept for the retry: {kept}"
        );
        download_verified(&srv.url, &dest, size, &digest, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert!(!part_path(&dest).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn server_without_range_support_restarts_from_zero() {
        let data = body(300_000);
        let srv = serve(data.clone(), Mode::NoRanges);
        let root = tmp("norange");
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("f");
        std::fs::write(part_path(&dest), &data[..1000]).unwrap();
        download_verified(&srv.url, &dest, data.len() as u64, &sha(&data), &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupt_leftover_part_is_discarded_and_redownloaded() {
        let data = body(300_000);
        let srv = serve(data.clone(), Mode::Ranges);
        let root = tmp("corrupt-part");
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("f");
        std::fs::write(part_path(&dest), vec![0xAAu8; 50_000]).unwrap();
        download_verified(&srv.url, &dest, data.len() as u64, &sha(&data), &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(
            srv.hits.load(Ordering::SeqCst),
            2,
            "one resumed attempt that failed its checksum, then a clean one"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn wrong_checksum_is_an_error_and_leaves_nothing_behind() {
        let data = body(100_000);
        let srv = serve(data.clone(), Mode::Ranges);
        let root = tmp("badsum");
        let dest = root.join("f");
        let err = download_verified(
            &srv.url,
            &dest,
            data.len() as u64,
            &sha(b"something else"),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("checksum"), "{err:#}");
        assert!(!dest.exists() && !part_path(&dest).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn http_error_is_reported() {
        let srv = serve(vec![], Mode::Missing);
        let root = tmp("404");
        let dest = root.join("f");
        assert!(download_verified(&srv.url, &dest, 10, &sha(b"x"), &mut |_| {}).is_err());
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ensure_files_downloads_everything_and_reports_overall_progress() {
        let (a, b) = (body(200_000), body(50_000));
        let sa = serve(a.clone(), Mode::Ranges);
        let root = tmp("ensure");
        let files = [
            ModelFile {
                name: "a.bin",
                size: a.len() as u64,
                sha256: Box::leak(sha(&a).into_boxed_str()),
            },
            ModelFile {
                name: "b.bin",
                size: b.len() as u64,
                sha256: Box::leak(sha(&b).into_boxed_str()),
            },
        ];
        // base_url/<name> → the test server answers every path with the same body, so serve `a` for a.bin only
        let base = sa.url.trim_end_matches("/f").to_string();
        let mut seen = vec![];
        let err = ensure_files(&root, &base, &files, &mut |p| seen.push(p)).unwrap_err();
        // b.bin has different bytes than the server sends: the checksum catches it.
        assert!(
            format!("{err:#}").contains("b.bin") || format!("{err:#}").contains("expected"),
            "{err:#}"
        );
        assert!(
            root.join("a.bin").is_file(),
            "the first file finished and stays"
        );
        assert!(!root.join("b.bin").exists());
        assert!(
            seen.windows(2).all(|w| w[0] <= w[1]),
            "progress never goes backwards: {seen:?}"
        );
        assert!(seen.iter().all(|p| (0.0..=1.0).contains(p)));
        assert!(files_ok(&root, &files[..1]) && !files_ok(&root, &files));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn sparse(path: &Path, len: u64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::File::create(path).unwrap().set_len(len).unwrap();
    }

    #[test]
    fn installed_means_every_file_is_complete_verified_and_the_vad_is_there() {
        let spec = find("parakeet-tdt-0.6b-v2-int8").unwrap();
        let root = tmp("installed");
        assert!(
            !is_present(spec, &root) && !is_installed(spec, &root),
            "empty profile"
        );
        let dir = dir_for(spec, &root);
        for f in spec.files {
            sparse(&dir.join(f.name), f.size);
        }
        assert!(
            !is_present(spec, &root),
            "model files without the VAD aren't complete"
        );
        sparse(&root.join(VAD_FILE), VAD_SIZE);
        assert!(is_present(spec, &root));
        assert!(
            !is_installed(spec, &root),
            "right sizes, but nobody has checked the bytes"
        );
        // record a passing check for every file
        let all: Vec<PathBuf> = spec
            .files
            .iter()
            .map(|f| dir.join(f.name))
            .chain([root.join(VAD_FILE)])
            .collect();
        for p in &all {
            std::fs::write(sidecar(p), stamp(p).unwrap()).unwrap();
        }
        assert!(is_installed(spec, &root));
        // a truncated encoder (e.g. a copy interrupted by a crash) isn't ready
        sparse(&dir.join(spec.encoder), 1000);
        assert!(!is_present(spec, &root) && !is_installed(spec, &root));
        // a leftover .part alone isn't ready either
        std::fs::remove_file(dir.join(spec.encoder)).unwrap();
        sparse(&dir.join("encoder.int8.onnx.part"), spec.files[0].size);
        assert!(!is_present(spec, &root) && !is_installed(spec, &root));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_installed_removes_damaged_files_so_retry_replaces_them() {
        let spec = find("parakeet-tdt-0.6b-v2-int8").unwrap();
        let root = tmp("verify");
        let dir = dir_for(spec, &root);
        for f in spec.files {
            sparse(&dir.join(f.name), f.size);
        }
        sparse(&root.join(VAD_FILE), VAD_SIZE);
        // all-zero files have the right size but the wrong checksum
        let err = verify_installed(spec, &root).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("damaged") && msg.contains("Download again"),
            "{msg}"
        );
        assert!(!is_present(spec, &root), "damaged files were deleted");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn catalog_lists_the_files_the_runtime_loads() {
        for m in MODELS {
            let names: Vec<_> = m.files.iter().map(|f| f.name).collect();
            for needed in [m.encoder, m.decoder, m.joiner, "tokens.txt"] {
                assert!(names.contains(&needed), "{} lists {needed}", m.id);
            }
            assert!(m.files.iter().all(|f| f.sha256.len() == 64 && f.size > 0));
            assert!(
                m.base_url.contains("/resolve/") && !m.base_url.ends_with("/main"),
                "pinned to a commit"
            );
        }
    }

    #[test]
    fn same_size_but_corrupt_file_is_caught_and_replaced() {
        // An older build could leave a complete-length file whose bytes were
        // overwritten by a racing download. Size alone can't see that.
        let data = body(200_000);
        let srv = serve(data.clone(), Mode::Ranges);
        let root = tmp("damaged");
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("f");
        let mut bad = data.clone();
        bad[..1000].fill(0);
        std::fs::write(&dest, &bad).unwrap();
        assert!(
            file_ok(&dest, data.len() as u64) && !verified(&dest, data.len() as u64, &sha(&data))
        );
        download_verified(&srv.url, &dest, data.len() as u64, &sha(&data), &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn verified_hashes_once_then_trusts_the_sidecar_until_the_file_changes() {
        let data = body(50_000);
        let root = tmp("sidecar");
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("f");
        std::fs::write(&dest, &data).unwrap();
        let (size, digest) = (data.len() as u64, sha(&data));
        assert!(verified(&dest, size, &digest));
        assert!(sidecar(&dest).exists());
        // a wrong digest is trusted because the sidecar matches this exact file…
        assert!(verified(&dest, size, "0000"));
        // …until the file changes (same length, different bytes, new mtime)
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut other = data.clone();
        other[0] ^= 0xff;
        std::fs::write(&dest, &other).unwrap();
        assert!(!verified(&dest, size, &digest));
        let _ = std::fs::remove_dir_all(&root);
    }
}
