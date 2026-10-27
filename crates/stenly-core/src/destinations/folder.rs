//! Local Markdown folders: the configured notes folder (built in; the
//! pipeline writes it after every meeting) and extra folders.

use super::{AutoRule, Delivered, Destination, DestinationConfig, Kind, Payload};
use crate::config::{Config, OutputConfig};
use crate::util::expand_tilde;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

fn file_url(p: &Path) -> String {
    let s = p.to_string_lossy();
    let enc: String = s
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!("file://{enc}")
}

/// Write (or rewrite) the note at `previous` if it's still inside `dir`,
/// else at a fresh non-clobbering path.
fn write_into(out: &OutputConfig, p: &Payload, md: &str, previous: Option<&str>) -> Result<PathBuf> {
    let dir = expand_tilde(&out.notes_dir);
    let path = match previous.map(PathBuf::from) {
        Some(prev) if prev.exists() && prev.parent() == Some(dir.as_path()) => prev,
        _ => crate::note::note_path(out, &p.meta),
    };
    crate::note::write_note(&path, md)?;
    Ok(path)
}

/// `[output] notes_dir`: where the pipeline already writes every note.
/// Sending rewrites it (e.g. after the file was deleted or moved).
pub struct NotesFolder {
    cfg: Config,
    name: String,
    auto: AutoRule,
}

impl NotesFolder {
    pub fn new(cfg: &Config) -> Self {
        let dir = expand_tilde(&cfg.output.notes_dir);
        let vault = crate::obsidian::vaults().into_iter().find(|v| dir.starts_with(&v.path));
        let name = match vault {
            Some(_) => "Obsidian".to_string(),
            None => "Notes folder".to_string(),
        };
        Self { cfg: cfg.clone(), name, auto: AutoRule::default() }
    }
}

impl Destination for NotesFolder {
    fn id(&self) -> &str {
        super::NOTES_FOLDER_ID
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> Kind {
        Kind::Folder
    }
    fn service(&self) -> String {
        crate::util::collapse_tilde(&expand_tilde(&self.cfg.output.notes_dir))
    }
    fn updates_in_place(&self) -> bool {
        true
    }
    fn ready(&self) -> Result<()> {
        if self.cfg.output.notes_dir.trim().is_empty() {
            bail!("no notes folder set (Settings → Notes & Storage)");
        }
        Ok(())
    }
    fn test(&self) -> Result<String> {
        test_dir(&expand_tilde(&self.cfg.output.notes_dir))
    }
    fn send(&self, p: &Payload, previous: Option<&str>) -> Result<Delivered> {
        let md = p.full_markdown(&self.cfg);
        // The pipeline's own copy is the natural "previous" file.
        let prev = previous.map(str::to_string).or_else(|| p.meta.note_path.as_ref().map(|x| x.to_string_lossy().to_string()));
        let path = write_into(&self.cfg.output, p, &md, prev.as_deref())?;
        Ok(Delivered { url: Some(file_url(&path)), remote_id: Some(path.to_string_lossy().to_string()), detail: None, clipboard: None })
    }
    fn auto(&self) -> &AutoRule {
        // Already written after every meeting by the pipeline.
        &self.auto
    }
}

fn test_dir(dir: &Path) -> Result<String> {
    std::fs::create_dir_all(dir).map_err(|e| anyhow::anyhow!("can't create {}: {e}", dir.display()))?;
    let probe = dir.join(".stenly-write-test");
    std::fs::write(&probe, b"ok").map_err(|e| anyhow::anyhow!("can't write to {}: {e}", dir.display()))?;
    let _ = std::fs::remove_file(probe);
    Ok("Folder is writable".into())
}

/// An extra Markdown folder (a second vault, a shared drive…).
pub struct Folder {
    cfg: Config,
    d: DestinationConfig,
}

impl Folder {
    pub fn new(cfg: &Config, d: DestinationConfig) -> Self {
        Self { cfg: cfg.clone(), d }
    }
    fn output(&self) -> OutputConfig {
        let mut o = self.cfg.output.clone();
        o.notes_dir = self.d.folder.path.clone();
        if !self.d.folder.filename.trim().is_empty() {
            o.filename = self.d.folder.filename.clone();
        }
        if let Some(t) = self.d.folder.include_transcript {
            o.include_transcript = t;
        }
        o.daily_note.clear();
        o
    }
}

impl Destination for Folder {
    fn id(&self) -> &str {
        &self.d.id
    }
    fn name(&self) -> &str {
        &self.d.name
    }
    fn kind(&self) -> Kind {
        Kind::Folder
    }
    fn service(&self) -> String {
        crate::util::collapse_tilde(&expand_tilde(&self.d.folder.path))
    }
    fn updates_in_place(&self) -> bool {
        true
    }
    fn ready(&self) -> Result<()> {
        if self.d.folder.path.trim().is_empty() {
            bail!("choose a folder for “{}”", self.d.name);
        }
        Ok(())
    }
    fn test(&self) -> Result<String> {
        self.ready()?;
        test_dir(&expand_tilde(&self.d.folder.path))
    }
    fn send(&self, p: &Payload, previous: Option<&str>) -> Result<Delivered> {
        let out = self.output();
        let keep_audio = out.keep_audio_days != 0;
        let md = crate::note::render(&p.meta, &p.transcript, p.summary.as_ref(), keep_audio.then_some(p.dir.as_path()), out.include_transcript);
        let path = write_into(&out, p, &md, previous)?;
        Ok(Delivered { url: Some(file_url(&path)), remote_id: Some(path.to_string_lossy().to_string()), detail: None, clipboard: None })
    }
    fn auto(&self) -> &AutoRule {
        &self.d.auto
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destinations::testutil;

    #[test]
    fn writes_then_rewrites_same_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut d = DestinationConfig::new(Kind::Folder, "Archive");
        d.folder.path = tmp.path().join("notes").to_string_lossy().to_string();
        let f = Folder::new(&Config::default(), d);
        let p = testutil::payload();
        let a = f.send(&p, None).unwrap();
        let path = a.remote_id.clone().unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("# Acme pilot"));
        assert!(a.url.unwrap().starts_with("file://"));
        let b = f.send(&p, Some(&path)).unwrap();
        assert_eq!(b.remote_id.unwrap(), path);
        assert_eq!(std::fs::read_dir(tmp.path().join("notes")).unwrap().count(), 1);
        assert_eq!(f.test().unwrap(), "Folder is writable");
    }

    #[test]
    fn needs_a_path() {
        let f = Folder::new(&Config::default(), DestinationConfig::new(Kind::Folder, "X"));
        assert!(f.ready().is_err());
    }
}
