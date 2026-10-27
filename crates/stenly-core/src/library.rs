//! The prompt library: note templates and recipes, stored as plain Markdown
//! files with a small front-matter header.
//!
//! ```text
//! ---
//! name: Follow-up email
//! description: A short email to everyone on the call.
//! ---
//! Instructions in Markdown…
//! ```
//!
//! Built-ins ship inside the binary (`crates/stenly-core/library/…`). The
//! user's own files live in `<config dir>/templates/` and `<config dir>/recipes/`
//! (next to `config.toml`). A user file with the same id (file stem) as a
//! built-in overrides it; one with `hidden: true` hides it. Anything else in
//! the folder is a custom entry. Edit them in the app or any text editor.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Template,
    Recipe,
}

impl Kind {
    pub fn dir_name(self) -> &'static str {
        match self {
            Kind::Template => "templates",
            Kind::Recipe => "recipes",
        }
    }
    fn builtins(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Kind::Template => BUILTIN_TEMPLATES,
            Kind::Recipe => BUILTIN_RECIPES,
        }
    }
}

macro_rules! builtin {
    ($kind:literal, $($id:literal),* $(,)?) => {
        &[$(($id, include_str!(concat!("../library/", $kind, "/", $id, ".md")))),*]
    };
}

/// In menu order.
const BUILTIN_TEMPLATES: &[(&str, &str)] = builtin!(
    "templates",
    "general",
    "one-on-one",
    "stand-up",
    "team-meeting",
    "customer-discovery",
    "sales-call",
    "user-interview",
    "hiring-interview",
    "pitch",
    "brainstorm",
    "project-kickoff",
    "retro",
);

const BUILTIN_RECIPES: &[(&str, &str)] = builtin!(
    "recipes",
    "follow-up-email",
    "tldr-for-slack",
    "action-items-by-owner",
    "decisions-and-open-questions",
    "risks",
    "customer-objections",
    "product-feedback",
    "interview-scorecard",
    "coach-me",
    "questions-to-ask",
    "write-a-help-doc",
    "project-brief",
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Shipped with Stenly, unchanged.
    Builtin,
    /// A built-in the user has edited (their file overrides ours).
    Modified,
    /// Made by the user.
    Custom,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// File stem, e.g. `follow-up-email`.
    pub id: String,
    pub name: String,
    pub description: String,
    /// Markdown instructions (everything after the front matter).
    pub body: String,
    /// Other front-matter keys (`match`, `heading`, …), in file order.
    pub fields: Vec<(String, String)>,
    pub origin: Origin,
    pub hidden: bool,
}

impl Entry {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self { id: id.into(), name: name.into(), description: String::new(), body: String::new(), fields: vec![], origin: Origin::Custom, hidden: false }
    }

    pub fn field(&self, key: &str) -> Option<&str> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty())
    }

    pub fn set_field(&mut self, key: &str, value: &str) {
        match self.fields.iter_mut().find(|(k, _)| k == key) {
            Some(f) => f.1 = value.to_string(),
            None => self.fields.push((key.to_string(), value.to_string())),
        }
    }

    /// A comma-separated field as a list (`match: 1:1, one on one`).
    pub fn list(&self, key: &str) -> Vec<String> {
        self.field(key).map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
    }

    pub fn is_builtin(&self) -> bool {
        self.origin != Origin::Custom
    }
}

/// Parse a library file. A file without front matter is all body, named after its id.
pub fn parse(id: &str, text: &str) -> Entry {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut e = Entry::new(id, id);
    let (header, body) = match text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) {
        Some(rest) => {
            // closing fence: a line that is exactly "---"
            let mut end = None;
            let mut pos = 0;
            for line in rest.split_inclusive('\n') {
                if line.trim_end_matches(['\r', '\n']) == "---" {
                    end = Some((pos, pos + line.len()));
                    break;
                }
                pos += line.len();
            }
            match end {
                Some((a, b)) => (&rest[..a], &rest[b..]),
                None => ("", text),
            }
        }
        None => ("", text),
    };
    for line in header.lines() {
        let line = line.trim_end();
        if line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let key = k.trim().to_lowercase();
        let value = unquote(v.trim());
        match key.as_str() {
            "name" | "title" => e.name = value,
            "description" => e.description = value,
            "hidden" => e.hidden = matches!(value.to_lowercase().as_str(), "true" | "yes" | "1"),
            _ => e.fields.push((key, value)),
        }
    }
    if e.name.trim().is_empty() {
        e.name = id.to_string();
    }
    e.body = body.trim_matches(['\n', '\r']).trim_end().to_string();
    e
}

fn unquote(v: &str) -> String {
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        if let Ok(s) = serde_json::from_str::<String>(v) {
            return s;
        }
    }
    if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
        return v[1..v.len() - 1].replace("''", "'");
    }
    v.to_string()
}

/// Quote a front-matter value when YAML would otherwise misread it.
fn quote(v: &str) -> String {
    let needs = v.is_empty()
        || v != v.trim()
        || v.contains(": ")
        || v.contains(" #")
        || v.contains(['"', '\n', '\''])
        || v.ends_with(':')
        || v.starts_with(['[', '{', '&', '*', '!', '|', '>', '%', '@', '`', '-', '?', '#'])
        // "1:1" reads as a sexagesimal number in YAML 1.1
        || v.chars().all(|c| c.is_ascii_digit() || c == ':');
    if needs { serde_json::to_string(v).unwrap_or_default() } else { v.to_string() }
}

pub fn serialize(e: &Entry) -> String {
    let mut s = String::from("---\n");
    s.push_str(&format!("name: {}\n", quote(&e.name)));
    if !e.description.is_empty() {
        s.push_str(&format!("description: {}\n", quote(&e.description)));
    }
    for (k, v) in &e.fields {
        if !v.is_empty() {
            s.push_str(&format!("{k}: {}\n", quote(v)));
        }
    }
    if e.hidden {
        s.push_str("hidden: true\n");
    }
    s.push_str("---\n\n");
    s.push_str(e.body.trim());
    s.push('\n');
    s
}

/// One kind of library entry (templates or recipes) backed by a folder.
#[derive(Debug, Clone)]
pub struct Library {
    pub kind: Kind,
    pub dir: PathBuf,
}

/// Folder next to `config.toml` (`~/Library/Application Support/stenly/templates`).
pub fn default_dir(kind: Kind) -> PathBuf {
    let cfg = crate::config::config_path();
    cfg.parent().map(Path::to_path_buf).unwrap_or_else(crate::config::data_dir).join(kind.dir_name())
}

impl Library {
    pub fn templates() -> Self {
        Self::at(Kind::Template, default_dir(Kind::Template))
    }
    pub fn recipes() -> Self {
        Self::at(Kind::Recipe, default_dir(Kind::Recipe))
    }
    pub fn at(kind: Kind, dir: impl Into<PathBuf>) -> Self {
        Self { kind, dir: dir.into() }
    }

    pub fn builtin(&self, id: &str) -> Option<Entry> {
        self.kind.builtins().iter().find(|(i, _)| *i == id).map(|(i, t)| {
            let mut e = parse(i, t);
            e.origin = Origin::Builtin;
            e
        })
    }

    fn user_files(&self) -> Vec<Entry> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return vec![] };
        let mut out: Vec<Entry> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("md")))
            .filter_map(|p| {
                let id = p.file_stem()?.to_string_lossy().to_string();
                if id.starts_with('.') || id.eq_ignore_ascii_case("readme") {
                    return None;
                }
                let text = std::fs::read_to_string(&p).ok()?;
                Some(parse(&id, &text))
            })
            .collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
        out
    }

    /// Everything, including hidden built-ins: built-ins first (in their
    /// shipped order, with the user's edits applied), then custom ones by name.
    pub fn all_with_hidden(&self) -> Vec<Entry> {
        let mut user = self.user_files();
        let mut out = Vec::new();
        for (id, text) in self.kind.builtins() {
            match user.iter().position(|u| u.id == *id) {
                Some(i) => {
                    let mut e = user.remove(i);
                    if e.hidden && e.body.is_empty() {
                        // a bare "hidden: true" marker: keep our text
                        let mut b = parse(id, text);
                        b.hidden = true;
                        b.origin = Origin::Builtin;
                        out.push(b);
                    } else {
                        e.origin = Origin::Modified;
                        out.push(e);
                    }
                }
                None => {
                    let mut e = parse(id, text);
                    e.origin = Origin::Builtin;
                    out.push(e);
                }
            }
        }
        for mut u in user {
            u.origin = Origin::Custom;
            out.push(u);
        }
        out
    }

    /// Visible entries in display order.
    pub fn all(&self) -> Vec<Entry> {
        self.all_with_hidden().into_iter().filter(|e| !e.hidden).collect()
    }

    pub fn get(&self, id: &str) -> Option<Entry> {
        self.all_with_hidden().into_iter().find(|e| e.id == id)
    }

    /// Look up by id, exact name, or a unique name/id fragment (case-insensitive).
    pub fn find(&self, query: &str) -> Result<Entry> {
        let q = query.trim().to_lowercase();
        let all = self.all();
        if let Some(e) = all.iter().find(|e| e.id == q || e.name.to_lowercase() == q) {
            return Ok(e.clone());
        }
        let slug = crate::util::slug(&q);
        let hits: Vec<&Entry> = all.iter().filter(|e| e.name.to_lowercase().contains(&q) || (!slug.is_empty() && e.id.contains(&slug))).collect();
        match hits.as_slice() {
            [one] => Ok((*one).clone()),
            [] => anyhow::bail!("no {} called {query:?} (try one of: {})", self.noun(), all.iter().map(|e| e.id.as_str()).collect::<Vec<_>>().join(", ")),
            many => anyhow::bail!("{query:?} matches several {}s: {}", self.noun(), many.iter().map(|e| e.name.as_str()).collect::<Vec<_>>().join(", ")),
        }
    }

    fn noun(&self) -> &'static str {
        match self.kind {
            Kind::Template => "template",
            Kind::Recipe => "recipe",
        }
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.md"))
    }

    /// Write an entry to the user folder (creating or overriding).
    pub fn save(&self, e: &Entry) -> Result<PathBuf> {
        anyhow::ensure!(!e.id.is_empty() && !e.id.contains(['/', '\\']) && !e.id.starts_with('.'), "invalid id {:?}", e.id);
        std::fs::create_dir_all(&self.dir).with_context(|| format!("creating {}", self.dir.display()))?;
        let path = self.path(&e.id);
        let tmp = self.dir.join(format!(".{}.md.tmp", e.id));
        std::fs::write(&tmp, serialize(e))?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// Custom entries are removed; built-ins are hidden (restore with [`Library::reset`]).
    pub fn delete(&self, id: &str) -> Result<()> {
        if let Some(b) = self.builtin(id) {
            let mut marker = Entry::new(id, b.name);
            marker.hidden = true;
            self.save(&marker)?;
            return Ok(());
        }
        let p = self.path(id);
        if p.exists() {
            std::fs::remove_file(&p).with_context(|| format!("removing {}", p.display()))?;
        }
        Ok(())
    }

    /// Put a built-in back the way it shipped (drops the user's edits/hiding).
    pub fn reset(&self, id: &str) -> Result<()> {
        if self.builtin(id).is_some() {
            let p = self.path(id);
            if p.exists() {
                std::fs::remove_file(p)?;
            }
        }
        Ok(())
    }

    /// An unused id for `name` (`follow-up-email`, `follow-up-email-2`, …).
    pub fn new_id(&self, name: &str) -> String {
        let base = match crate::util::slug(name) {
            s if s.is_empty() => self.noun().to_string(),
            s => s,
        };
        let taken = |id: &str| self.builtin(id).is_some() || self.path(id).exists();
        if !taken(&base) {
            return base;
        }
        (2..).map(|n| format!("{base}-{n}")).find(|id| !taken(id)).unwrap()
    }

    /// A custom copy named "… copy".
    pub fn duplicate(&self, id: &str) -> Result<Entry> {
        let src = self.get(id).with_context(|| format!("no {} {id:?}", self.noun()))?;
        let name = format!("{} copy", src.name);
        let mut e = Entry { id: self.new_id(&name), name, origin: Origin::Custom, hidden: false, ..src };
        e.fields.retain(|(k, _)| k != "match");
        self.save(&e)?;
        Ok(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse() {
        for kind in [Kind::Template, Kind::Recipe] {
            let lib = Library::at(kind, "/nonexistent");
            let all = lib.all();
            assert_eq!(all.len(), kind.builtins().len());
            for e in all {
                assert!(!e.name.is_empty() && e.name != e.id, "{} has no name", e.id);
                assert!(!e.description.is_empty(), "{} has no description", e.id);
                assert!(e.body.len() > 40, "{} has no instructions", e.id);
                assert_eq!(e.origin, Origin::Builtin);
            }
        }
    }

    #[test]
    fn round_trips() {
        let mut e = Entry::new("one", "1:1");
        e.description = "Weekly: you and a report".into();
        e.set_field("match", "1:1, one on one");
        e.body = "## Updates\nWhat they did.".into();
        let text = serialize(&e);
        assert!(text.starts_with("---\nname: \"1:1\"\ndescription: \"Weekly: you and a report\"\nmatch: 1:1, one on one\n---\n"));
        let back = parse("one", &text);
        assert_eq!(back, Entry { origin: Origin::Custom, ..e });
        assert_eq!(back.list("match"), ["1:1", "one on one"]);
    }

    #[test]
    fn tolerates_hand_written_files() {
        let e = parse("x", "no front matter at all\n\nsecond para\n");
        assert_eq!(e.name, "x");
        assert_eq!(e.body, "no front matter at all\n\nsecond para");
        let e = parse("y", "---\r\nname: 'It''s'\r\n# comment\r\nheading: Notes\r\n---\r\nbody\r\n");
        assert_eq!(e.name, "It's");
        assert_eq!(e.field("heading"), Some("Notes"));
        assert_eq!(e.body, "body");
        // unterminated front matter: treat it all as body
        let e = parse("z", "---\nname: oops\n");
        assert_eq!(e.name, "z");
    }

    #[test]
    fn user_files_override_hide_and_add() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::at(Kind::Recipe, dir.path());
        let n = lib.all().len();

        // edit a built-in
        let mut e = lib.get("follow-up-email").unwrap();
        e.body = "Shorter, please.".into();
        lib.save(&e).unwrap();
        let got = lib.get("follow-up-email").unwrap();
        assert_eq!((got.origin, got.body.as_str()), (Origin::Modified, "Shorter, please."));
        lib.reset("follow-up-email").unwrap();
        assert_eq!(lib.get("follow-up-email").unwrap().origin, Origin::Builtin);

        // hide a built-in, then restore it
        lib.delete("risks").unwrap();
        assert_eq!(lib.all().len(), n - 1);
        assert!(lib.get("risks").unwrap().hidden);
        lib.reset("risks").unwrap();
        assert_eq!(lib.all().len(), n);

        // custom: create, duplicate, find, delete
        let mut c = Entry::new(lib.new_id("My recipe"), "My recipe");
        c.body = "Do the thing.".into();
        lib.save(&c).unwrap();
        assert_eq!(c.id, "my-recipe");
        assert_eq!(lib.new_id("My recipe"), "my-recipe-2");
        assert_eq!(lib.new_id("Risks"), "risks-2");
        let d = lib.duplicate("my-recipe").unwrap();
        assert_eq!((d.id.as_str(), d.name.as_str()), ("my-recipe-copy", "My recipe copy"));
        assert_eq!(lib.all().len(), n + 2);
        assert_eq!(lib.all().last().unwrap().id, "my-recipe-copy");
        assert!(lib.find("my recipe").is_ok(), "exact name wins over fragments");
        assert!(lib.find("recipe copy").is_ok());
        assert!(lib.find("nope").is_err());
        lib.delete("my-recipe").unwrap();
        lib.delete("my-recipe-copy").unwrap();
        assert_eq!(lib.all().len(), n);
        assert!(lib.save(&Entry::new("../x", "x")).is_err());
    }

    #[test]
    fn finds_by_name_and_fragment() {
        let lib = Library::at(Kind::Recipe, "/nonexistent");
        assert_eq!(lib.find("TL;DR for Slack").unwrap().id, "tldr-for-slack");
        assert_eq!(lib.find("follow-up-email").unwrap().id, "follow-up-email");
        assert_eq!(lib.find("scorecard").unwrap().id, "interview-scorecard");
    }
}
