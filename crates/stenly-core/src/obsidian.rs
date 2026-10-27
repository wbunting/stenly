//! Find the user's Obsidian vaults (for one-click "save notes to my vault").

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct Vault {
    pub name: String,
    pub path: PathBuf,
}

fn obsidian_json() -> Option<PathBuf> {
    let base = directories::BaseDirs::new()?;
    #[cfg(target_os = "macos")]
    let p = base.home_dir().join("Library/Application Support/obsidian/obsidian.json");
    #[cfg(not(target_os = "macos"))]
    let p = base.config_dir().join("obsidian/obsidian.json");
    Some(p)
}

pub fn vaults() -> Vec<Vault> {
    let Some(path) = obsidian_json() else { return vec![] };
    let Ok(text) = std::fs::read_to_string(path) else { return vec![] };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return vec![] };
    let mut out: Vec<(i64, Vault)> = v["vaults"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, x)| {
            let path = PathBuf::from(x["path"].as_str()?);
            path.is_dir().then(|| {
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                (x["ts"].as_i64().unwrap_or(0), Vault { name, path })
            })
        })
        .collect();
    out.sort_by_key(|(ts, _)| -ts);
    out.into_iter().map(|(_, v)| v).collect()
}
