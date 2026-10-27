//! Notion: one page per meeting in a database you choose, using the REST API
//! directly (https://developers.notion.com/reference) with an internal
//! integration token kept in the Keychain.
//!
//! - Properties: title, date, attendees, tags and meeting URL are mapped onto
//!   columns you pick; columns that don't exist are skipped (and reported).
//! - Body: the note's Markdown as Notion blocks, plus the transcript in a
//!   toggle if asked.
//! - Relation (optional): fill e.g. "Company" with the records of a related
//!   database whose domain / email / name matches the attendees.
//! - Re-sending updates the page from the last send (properties + body)
//!   instead of creating another one; if that page was deleted, a new one is made.

use super::config::{MatchBy, NotionOpts, RelationMatch};
use super::http::{request, snippet};
use super::markdown::{plain_rich_text, to_notion_blocks};
use super::{AutoRule, Delivered, Destination, DestinationConfig, Kind, Payload, people};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

pub const API_BASE: &str = "https://api.notion.com/v1";
/// Single-data-source databases work unchanged on this version.
pub const NOTION_VERSION: &str = "2022-06-28";
/// Children per request (Notion's limit).
const BATCH: usize = 100;

/// `https://www.notion.so/ws/Meetings-0123…cdef?v=…` or a bare id → dashed UUID.
pub fn parse_id(s: &str) -> Option<String> {
    let s = s.trim();
    let s = s.split(['?', '#']).next().unwrap_or(s).trim_end_matches('/');
    // The id is the last 32 hex digits of the last path segment ("Meetings-<id>").
    let tail: String = s.rsplit('/').next().unwrap_or(s).chars().filter(|c| *c != '-').collect();
    if tail.len() < 32 || !tail.is_ascii() {
        return None;
    }
    let c = tail[tail.len() - 32..].to_ascii_lowercase();
    if !c.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("{}-{}-{}-{}-{}", &c[..8], &c[8..12], &c[12..16], &c[16..20], &c[20..]))
}

pub struct Client {
    base: String,
    token: String,
}

impl Client {
    pub fn new(base: &str, token: &str) -> Self {
        let base = if base.trim().is_empty() { API_BASE } else { base.trim() };
        Self { base: base.trim_end_matches('/').to_string(), token: token.trim().to_string() }
    }

    pub fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = format!("{}{path}", self.base);
        let auth = format!("Bearer {}", self.token);
        let headers = [("Authorization", auth.as_str()), ("Notion-Version", NOTION_VERSION)];
        let mut attempt = 0;
        loop {
            let r = request(method, &url, &headers, body, "Notion")?;
            if r.status == 429 && attempt < 3 {
                attempt += 1;
                // Notion asks for ~3 req/s; back off and retry.
                std::thread::sleep(std::time::Duration::from_millis(1000 * attempt));
                continue;
            }
            if r.ok() {
                return Ok(r.json());
            }
            let j = r.json();
            let code = j["code"].as_str().unwrap_or_default();
            let msg = j["message"].as_str().map(str::to_string).unwrap_or_else(|| snippet(&r.text));
            return Err(NotionError { status: r.status, code: code.to_string(), message: msg }.into());
        }
    }

    pub fn me(&self) -> Result<Value> {
        self.call("GET", "/users/me", None)
    }

    pub fn database(&self, id: &str) -> Result<Value> {
        self.call("GET", &format!("/databases/{id}"), None)
    }

    /// Databases shared with the integration: (id, title).
    pub fn databases(&self) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..10 {
            let mut body = json!({ "filter": { "property": "object", "value": "database" }, "page_size": 100 });
            if let Some(c) = &cursor {
                body["start_cursor"] = json!(c);
            }
            let r = self.call("POST", "/search", Some(&body))?;
            for d in r["results"].as_array().into_iter().flatten() {
                let id = d["id"].as_str().unwrap_or_default().to_string();
                let title = plain(&d["title"]);
                out.push((id, if title.is_empty() { "Untitled".into() } else { title }));
            }
            match r["next_cursor"].as_str() {
                Some(c) if r["has_more"].as_bool() == Some(true) => cursor = Some(c.to_string()),
                _ => break,
            }
        }
        out.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
        Ok(out)
    }

    fn query(&self, db: &str, filter: Value, limit: usize) -> Result<Vec<Value>> {
        let r = self.call("POST", &format!("/databases/{db}/query"), Some(&json!({ "filter": filter, "page_size": limit.min(100) })))?;
        Ok(r["results"].as_array().cloned().unwrap_or_default())
    }

    fn children(&self, block: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..50 {
            let q = match &cursor {
                Some(c) => format!("/blocks/{block}/children?page_size=100&start_cursor={c}"),
                None => format!("/blocks/{block}/children?page_size=100"),
            };
            let r = self.call("GET", &q, None)?;
            out.extend(r["results"].as_array().into_iter().flatten().filter_map(|b| b["id"].as_str().map(str::to_string)));
            match r["next_cursor"].as_str() {
                Some(c) if r["has_more"].as_bool() == Some(true) => cursor = Some(c.to_string()),
                _ => break,
            }
        }
        Ok(out)
    }

    /// Append blocks in batches; returns the ids of the created top-level blocks.
    fn append(&self, block: &str, blocks: &[Value]) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        for chunk in blocks.chunks(BATCH) {
            let r = self.call("PATCH", &format!("/blocks/{block}/children"), Some(&json!({ "children": chunk })))?;
            ids.extend(r["results"].as_array().into_iter().flatten().filter_map(|b| b["id"].as_str().map(str::to_string)));
        }
        Ok(ids)
    }

    /// Workspace members' emails → user ids (for a people property).
    fn users_by_email(&self) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..10 {
            let q = match &cursor {
                Some(c) => format!("/users?page_size=100&start_cursor={c}"),
                None => "/users?page_size=100".to_string(),
            };
            let r = self.call("GET", &q, None)?;
            for u in r["results"].as_array().into_iter().flatten() {
                if let (Some(id), Some(email)) = (u["id"].as_str(), u["person"]["email"].as_str()) {
                    out.push((email.to_ascii_lowercase(), id.to_string()));
                }
            }
            match r["next_cursor"].as_str() {
                Some(c) if r["has_more"].as_bool() == Some(true) => cursor = Some(c.to_string()),
                _ => break,
            }
        }
        Ok(out)
    }
}

#[derive(Debug)]
pub struct NotionError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for NotionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.status, self.code.as_str()) {
            (401, _) => write!(f, "Notion rejected the integration token"),
            (404, _) | (_, "object_not_found") => {
                write!(f, "Notion can't find it. Share the database with your integration (••• → Connections) and check the link")
            }
            (403, _) => write!(f, "the integration isn't allowed to do that ({})", self.message),
            (s, _) => write!(f, "Notion error {s}: {}", self.message),
        }
    }
}

impl std::error::Error for NotionError {}

fn is_not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<NotionError>().is_some_and(|n| n.status == 404 || n.code == "object_not_found")
}

/// Concatenated plain text of a rich-text / title array.
pub fn plain(v: &Value) -> String {
    v.as_array().into_iter().flatten().filter_map(|t| t["plain_text"].as_str().or(t["text"]["content"].as_str())).collect()
}

/// A database's columns: (name, type), title first, then alphabetical.
pub fn columns(db: &Value) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = db["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, p)| (k.clone(), p["type"].as_str().unwrap_or_default().to_string()))
        .collect();
    v.sort_by(|a, b| (a.1 != "title").cmp(&(b.1 != "title")).then(a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    v
}

fn prop_type<'a>(db: &'a Value, name: &str) -> Option<&'a str> {
    db["properties"][name]["type"].as_str()
}

fn title_column(db: &Value) -> Option<String> {
    columns(db).into_iter().find(|(_, t)| t == "title").map(|(n, _)| n)
}

/// The value of a page property as text, for matching.
fn prop_text(page: &Value, name: &str) -> String {
    let p = &page["properties"][name];
    match p["type"].as_str().unwrap_or_default() {
        "title" => plain(&p["title"]),
        "rich_text" => plain(&p["rich_text"]),
        "url" => p["url"].as_str().unwrap_or_default().to_string(),
        "email" => p["email"].as_str().unwrap_or_default().to_string(),
        "phone_number" => p["phone_number"].as_str().unwrap_or_default().to_string(),
        "multi_select" => p["multi_select"].as_array().into_iter().flatten().filter_map(|o| o["name"].as_str()).collect::<Vec<_>>().join(", "),
        "select" => p["select"]["name"].as_str().unwrap_or_default().to_string(),
        "formula" => p["formula"]["string"].as_str().unwrap_or_default().to_string(),
        _ => String::new(),
    }
}

/// Filter "column contains text" for any text-like column type.
fn contains_filter(column: &str, kind: &str, text: &str) -> Option<Value> {
    let k = match kind {
        "title" | "rich_text" | "url" | "email" | "phone_number" => kind,
        _ => return None,
    };
    Some(json!({ "property": column, k: { "contains": text } }))
}

/// What a send changed beyond the page itself.
#[derive(Debug, Default)]
struct Notes {
    skipped: Vec<String>,
    linked: Vec<String>,
}

pub struct NotionDestination {
    d: DestinationConfig,
}

impl NotionDestination {
    pub fn new(d: DestinationConfig) -> Self {
        Self { d }
    }
    fn opts(&self) -> &NotionOpts {
        &self.d.notion
    }
    fn client(&self) -> Result<Client> {
        let Some(t) = super::secret_for(&self.d, "") else { bail!("add the Notion integration token for “{}”", self.d.name) };
        Ok(Client::new(&self.opts().api_base, &t))
    }
    fn database_id(&self) -> Result<String> {
        parse_id(&self.opts().database_id).with_context(|| format!("choose a Notion database for “{}”", self.d.name))
    }

    /// Page properties for this meeting, per the column mapping.
    fn properties(&self, c: &Client, db: &Value, p: &Payload, notes: &mut Notes) -> Result<Value> {
        let o = self.opts();
        let mut props = serde_json::Map::new();
        let title_col = match o.title_property.trim() {
            "" => title_column(db).context("the Notion database has no title column")?,
            t => t.to_string(),
        };
        props.insert(title_col, json!({ "title": plain_rich_text(&p.meta.title) }));

        let mut set = |name: &str, f: &dyn Fn(&str) -> Option<Value>, notes: &mut Notes| {
            let name = name.trim();
            if name.is_empty() {
                return;
            }
            match prop_type(db, name).and_then(|t| f(t).map(|v| (t, v))) {
                Some((_, v)) => {
                    props.insert(name.to_string(), v);
                }
                None => notes.skipped.push(name.to_string()),
            }
        };

        let iso = |t: chrono::DateTime<chrono::Local>| t.format("%Y-%m-%dT%H:%M:%S%:z").to_string();
        let start = iso(p.meta.started);
        let end = p.meta.ended.map(iso);
        set(&o.date_property, &|t| (t == "date").then(|| json!({ "date": { "start": start, "end": end } })), notes);

        let others = people::others(&p.meta);
        let names: Vec<String> = others.iter().map(|a| people::name(a)).collect();
        let emails: Vec<String> = others.iter().map(|a| a.email.clone()).collect();
        let user_ids: Vec<String> = if prop_type(db, o.attendees_property.trim()) == Some("people") && !emails.is_empty() {
            let users = c.users_by_email().unwrap_or_default();
            emails.iter().filter_map(|e| users.iter().find(|(u, _)| u.eq_ignore_ascii_case(e)).map(|(_, id)| id.clone())).collect()
        } else {
            vec![]
        };
        if !others.is_empty() {
            set(
                &o.attendees_property,
                &|t| match t {
                    "multi_select" => Some(json!({ "multi_select": names.iter().map(|n| json!({ "name": n.replace(',', " ").chars().take(100).collect::<String>() })).collect::<Vec<_>>() })),
                    "rich_text" => Some(json!({ "rich_text": plain_rich_text(&names.join(", ")) })),
                    "email" => emails.first().map(|e| json!({ "email": e })),
                    "people" => Some(json!({ "people": user_ids.iter().map(|id| json!({ "object": "user", "id": id })).collect::<Vec<_>>() })),
                    _ => None,
                },
                notes,
            );
        }
        let tags: Vec<String> = p.summary.as_ref().map(|s| s.tags.clone()).unwrap_or_default();
        if !tags.is_empty() {
            set(&o.tags_property, &|t| (t == "multi_select").then(|| json!({ "multi_select": tags.iter().map(|n| json!({ "name": n.replace(',', " ") })).collect::<Vec<_>>() })), notes);
        }
        if let Some(url) = p.meta.event.as_ref().and_then(|e| e.link.as_ref()).map(|l| l.url.clone()) {
            set(&o.url_property, &|t| (t == "url").then(|| json!({ "url": url })), notes);
        }
        if let Some(rel) = &o.relation
            && !rel.property.trim().is_empty()
        {
            match prop_type(db, rel.property.trim()) {
                Some("relation") => {
                    let target = parse_id(&rel.database_id).or_else(|| db["properties"][rel.property.trim()]["relation"]["database_id"].as_str().map(str::to_string));
                    match target {
                        Some(target) => {
                            let found = match_related(c, &target, rel, &p.meta)?;
                            if !found.is_empty() {
                                notes.linked = found.iter().map(|(_, t)| t.clone()).collect();
                                props.insert(rel.property.trim().to_string(), json!({ "relation": found.iter().map(|(id, _)| json!({ "id": id })).collect::<Vec<_>>() }));
                            }
                        }
                        None => notes.skipped.push(rel.property.clone()),
                    }
                }
                _ => notes.skipped.push(rel.property.clone()),
            }
        }
        Ok(Value::Object(props))
    }

    fn body_blocks(&self, p: &Payload) -> (Vec<Value>, Option<(Value, Vec<Value>)>) {
        let mut blocks = to_notion_blocks(&p.body_markdown());
        if blocks.is_empty() {
            blocks.push(json!({ "object": "block", "type": "paragraph", "paragraph": { "rich_text": plain_rich_text("No AI notes for this meeting.") } }));
        }
        let transcript = (self.opts().include_transcript && !p.transcript.is_empty()).then(|| {
            let toggle = json!({ "object": "block", "type": "toggle", "toggle": { "rich_text": plain_rich_text(&format!("Transcript ({} words)", p.transcript.word_count())) } });
            let lines: Vec<Value> = p
                .transcript
                .segments
                .iter()
                .map(|s| json!({ "object": "block", "type": "paragraph", "paragraph": { "rich_text": plain_rich_text(&format!("[{}] {}: {}", crate::util::fmt_timestamp(s.start), super::speaker_name(s), s.text)) } }))
                .collect();
            (toggle, lines)
        });
        (blocks, transcript)
    }

    fn write_body(&self, c: &Client, page: &str, p: &Payload) -> Result<()> {
        let (blocks, transcript) = self.body_blocks(p);
        c.append(page, &blocks)?;
        if let Some((toggle, lines)) = transcript {
            let ids = c.append(page, &[toggle])?;
            if let Some(t) = ids.first() {
                c.append(t, &lines)?;
            }
        }
        Ok(())
    }
}

/// Records of the related database matching the meeting's attendees: (page id, title).
fn match_related(c: &Client, db_id: &str, rel: &RelationMatch, meta: &crate::session::SessionMeta) -> Result<Vec<(String, String)>> {
    let db = c.database(db_id).context("reading the related database")?;
    let cols = columns(&db);
    let title_col = title_column(&db).unwrap_or_default();
    let pick = |prefer: &[&str], types: &[&str]| -> Option<(String, String)> {
        if !rel.match_property.trim().is_empty() {
            let n = rel.match_property.trim();
            return prop_type(&db, n).map(|t| (n.to_string(), t.to_string()));
        }
        prefer
            .iter()
            .find_map(|want| cols.iter().find(|(n, t)| n.to_lowercase().contains(want) && types.contains(&t.as_str())))
            .or_else(|| cols.iter().find(|(_, t)| types.contains(&t.as_str())))
            .cloned()
    };
    let mut found: Vec<(String, String)> = Vec::new();
    let push = |page: &Value, found: &mut Vec<(String, String)>| {
        if let Some(id) = page["id"].as_str()
            && !found.iter().any(|(x, _)| x == id)
        {
            found.push((id.to_string(), prop_text(page, &title_col)));
        }
    };
    match rel.match_by {
        MatchBy::Domain => {
            let Some((col, kind)) = pick(&["domain", "website", "url", "site", "email"], &["url", "rich_text", "email", "title"]) else { return Ok(vec![]) };
            for d in people::external_domains(meta).into_iter().take(10) {
                let Some(filter) = contains_filter(&col, &kind, &d) else { continue };
                for page in c.query(db_id, filter, 20)? {
                    let text = prop_text(&page, &col);
                    let hit = text.split([',', ';', ' ', '\n']).filter_map(people::host).any(|h| people::same_org(&h, &d));
                    if hit {
                        push(&page, &mut found);
                    }
                }
            }
        }
        MatchBy::Email => {
            let Some((col, kind)) = pick(&["email"], &["email", "rich_text", "url"]) else { return Ok(vec![]) };
            for a in people::others(meta).into_iter().take(15) {
                let Some(filter) = contains_filter(&col, &kind, &a.email) else { continue };
                for page in c.query(db_id, filter, 10)? {
                    if prop_text(&page, &col).split([',', ';', ' ', '\n']).any(|e| e.trim().eq_ignore_ascii_case(&a.email)) {
                        push(&page, &mut found);
                    }
                }
            }
        }
        MatchBy::Name => {
            let (col, kind) = pick(&[], &["title"]).unwrap_or((title_col.clone(), "title".into()));
            for a in people::others(meta).into_iter().take(15) {
                let Some(name) = a.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) else { continue };
                let Some(filter) = contains_filter(&col, &kind, name) else { continue };
                for page in c.query(db_id, filter, 10)? {
                    if prop_text(&page, &col).trim().eq_ignore_ascii_case(name) {
                        push(&page, &mut found);
                    }
                }
            }
        }
    }
    Ok(found)
}

impl Destination for NotionDestination {
    fn id(&self) -> &str {
        &self.d.id
    }
    fn name(&self) -> &str {
        &self.d.name
    }
    fn kind(&self) -> Kind {
        Kind::Notion
    }
    fn updates_in_place(&self) -> bool {
        true
    }
    fn ready(&self) -> Result<()> {
        self.client()?;
        self.database_id()?;
        Ok(())
    }
    fn test(&self) -> Result<String> {
        let c = self.client()?;
        let me = c.me()?;
        let bot = me["name"].as_str().unwrap_or("the integration").to_string();
        let Ok(id) = self.database_id() else { return Ok(format!("Connected as {bot}. Now choose a database.")) };
        let db = c.database(&id)?;
        let title = plain(&db["title"]);
        let o = self.opts();
        let mut missing = Vec::new();
        for (name, want) in [(&o.title_property, "title"), (&o.date_property, "date"), (&o.tags_property, "multi_select"), (&o.url_property, "url")] {
            let n = name.trim();
            if !n.is_empty() && prop_type(&db, n) != Some(want) {
                missing.push(n.to_string());
            }
        }
        let a = o.attendees_property.trim();
        if !a.is_empty() && !matches!(prop_type(&db, a), Some("multi_select" | "rich_text" | "people" | "email")) {
            missing.push(a.to_string());
        }
        if let Some(r) = &o.relation
            && !r.property.trim().is_empty()
            && prop_type(&db, r.property.trim()) != Some("relation")
        {
            missing.push(r.property.trim().to_string());
        }
        if missing.is_empty() {
            Ok(format!("Connected to “{title}”"))
        } else {
            bail!("“{title}” has no matching column for {}", missing.iter().map(|m| format!("“{m}”")).collect::<Vec<_>>().join(", "))
        }
    }
    fn send(&self, p: &Payload, previous: Option<&str>) -> Result<Delivered> {
        let c = self.client()?;
        let db_id = self.database_id()?;
        let db = c.database(&db_id)?;
        let mut notes = Notes::default();
        let props = self.properties(&c, &db, p, &mut notes)?;

        // Update the page from last time if it's still there.
        let existing = match previous {
            Some(id) => match c.call("GET", &format!("/pages/{id}"), None) {
                Ok(pg) if pg["archived"].as_bool() != Some(true) && pg["in_trash"].as_bool() != Some(true) => Some(pg),
                Ok(_) => None,
                Err(e) if is_not_found(&e) => None,
                Err(e) => return Err(e),
            },
            None => None,
        };
        let (page, updated) = match existing {
            Some(pg) => {
                let id = pg["id"].as_str().unwrap_or_default().to_string();
                let pg = c.call("PATCH", &format!("/pages/{id}"), Some(&json!({ "properties": props })))?;
                for b in c.children(&id)? {
                    match c.call("DELETE", &format!("/blocks/{b}"), None) {
                        Ok(_) => {}
                        Err(e) if is_not_found(&e) => {}
                        Err(e) => return Err(e),
                    }
                }
                self.write_body(&c, &id, p)?;
                (pg, true)
            }
            None => {
                let (blocks, transcript) = self.body_blocks(p);
                let first: Vec<Value> = blocks.iter().take(BATCH).cloned().collect();
                let pg = c.call("POST", "/pages", Some(&json!({ "parent": { "database_id": db_id }, "properties": props, "children": first })))?;
                let id = pg["id"].as_str().context("Notion didn't return a page id")?.to_string();
                if blocks.len() > BATCH {
                    c.append(&id, &blocks[BATCH..])?;
                }
                if let Some((toggle, lines)) = transcript {
                    let ids = c.append(&id, &[toggle])?;
                    if let Some(t) = ids.first() {
                        c.append(t, &lines)?;
                    }
                }
                (pg, false)
            }
        };
        let id = page["id"].as_str().unwrap_or_default().to_string();
        let mut detail = vec![if updated { "Updated page".to_string() } else { "Created page".to_string() }];
        if !notes.linked.is_empty() {
            detail.push(format!("linked {}", notes.linked.join(", ")));
        }
        if !notes.skipped.is_empty() {
            detail.push(format!("skipped {}", notes.skipped.join(", ")));
        }
        Ok(Delivered { url: page["url"].as_str().map(str::to_string), remote_id: Some(id), detail: Some(detail.join(" · ")), clipboard: None })
    }
    fn auto(&self) -> &AutoRule {
        &self.d.auto
    }
}

/// Settings helpers (token from the form or the Keychain).
pub fn list_databases(api_base: &str, token: &str) -> Result<Vec<(String, String)>> {
    Client::new(api_base, token).databases()
}

/// What the settings form needs to map properties.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Schema {
    pub id: String,
    pub title: String,
    /// (column, type), title column first.
    pub columns: Vec<(String, String)>,
    /// Relation columns and the database each points at.
    pub relations: Vec<(String, String)>,
}

impl Schema {
    pub fn from_json(db: &Value) -> Self {
        let relations = db["properties"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, p)| p["type"] == "relation")
            .filter_map(|(k, p)| p["relation"]["database_id"].as_str().map(|d| (k.clone(), d.to_string())))
            .collect();
        Self { id: db["id"].as_str().unwrap_or_default().to_string(), title: plain(&db["title"]), columns: columns(db), relations }
    }
    /// Columns of one of `types`.
    pub fn of(&self, types: &[&str]) -> Vec<String> {
        self.columns.iter().filter(|(_, t)| types.contains(&t.as_str())).map(|(n, _)| n.clone()).collect()
    }
}

pub fn database_schema(api_base: &str, token: &str, db: &str) -> Result<Schema> {
    let id = parse_id(db).context("that isn't a Notion database link or ID")?;
    Ok(Schema::from_json(&Client::new(api_base, token).database(&id)?))
}

/// Fill in sensible column names for a database: its title column, and the
/// first date / multi-select / url column whose name suggests the field.
pub fn suggest_mapping(o: &mut NotionOpts, s: &Schema) {
    let find = |want: &[&str], types: &[&str]| -> String {
        let cols = s.of(types);
        want.iter().find_map(|w| cols.iter().find(|c| c.to_lowercase().contains(w)).cloned()).unwrap_or_default()
    };
    let keep = |cur: &str, types: &[&str]| s.of(types).iter().any(|c| c == cur);
    o.title_property = s.of(&["title"]).into_iter().next().unwrap_or_default();
    if !keep(&o.date_property, &["date"]) {
        o.date_property = find(&["date", "when", "time"], &["date"]);
    }
    if !keep(&o.attendees_property, &["multi_select", "rich_text", "people", "email"]) {
        o.attendees_property = find(&["attendee", "participant", "people", "who", "guest"], &["multi_select", "rich_text", "people"]);
    }
    if !keep(&o.tags_property, &["multi_select"]) {
        o.tags_property = find(&["tag", "topic", "label"], &["multi_select"]);
    }
    if !keep(&o.url_property, &["url"]) {
        o.url_property = find(&["meeting", "link", "url", "call"], &["url"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destinations::testutil::{self, MockServer, Req};
    use std::sync::{Arc, Mutex};

    const DB: &str = "11111111-2222-3333-4444-555555555555";
    const COMPANIES: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    #[test]
    fn parses_ids() {
        assert_eq!(parse_id("https://www.notion.so/acme/Meetings-0123456789abcdef0123456789ABCDEF?v=ffff").as_deref(), Some("01234567-89ab-cdef-0123-456789abcdef"));
        assert_eq!(parse_id("0123456789abcdef0123456789abcdef").as_deref(), Some("01234567-89ab-cdef-0123-456789abcdef"));
        assert_eq!(parse_id(DB).as_deref(), Some(DB));
        assert_eq!(parse_id("Meetings"), None);
        assert_eq!(parse_id(""), None);
    }

    fn meetings_db() -> Value {
        json!({ "object": "database", "id": DB, "title": [{ "plain_text": "Meetings" }], "properties": {
            "Name": { "type": "title" }, "Date": { "type": "date" }, "Attendees": { "type": "multi_select" },
            "Tags": { "type": "multi_select" }, "Company": { "type": "relation", "relation": { "database_id": COMPANIES } },
        }})
    }

    fn companies_db() -> Value {
        json!({ "object": "database", "id": COMPANIES, "title": [{ "plain_text": "Companies" }], "properties": {
            "Name": { "type": "title" }, "Website": { "type": "url" },
        }})
    }

    /// A fake Notion: one database, a Companies database with Acme, pages in memory.
    fn fake_notion() -> (MockServer, Arc<Mutex<Vec<String>>>) {
        let pages: Arc<Mutex<Vec<String>>> = Arc::default();
        let p2 = pages.clone();
        let srv = MockServer::start(move |r: &Req| {
            if r.header("Authorization") != Some("Bearer ntn_test") {
                return (401, r#"{"object":"error","status":401,"code":"unauthorized","message":"API token is invalid."}"#.into());
            }
            assert_eq!(r.header("Notion-Version"), Some(NOTION_VERSION));
            let path = r.path.as_str();
            match (r.method.as_str(), path) {
                ("GET", "/v1/users/me") => (200, r#"{"object":"user","name":"Stenly bot"}"#.into()),
                ("GET", p) if p == format!("/v1/databases/{DB}") => (200, meetings_db().to_string()),
                ("GET", p) if p == format!("/v1/databases/{COMPANIES}") => (200, companies_db().to_string()),
                ("POST", p) if p == format!("/v1/databases/{COMPANIES}/query") => {
                    let f = &r.json()["filter"];
                    let hit = f["url"]["contains"].as_str() == Some("acme.com");
                    let res = if hit {
                        json!([{ "id": "acme-page", "properties": { "Name": { "type": "title", "title": [{ "plain_text": "Acme Corp" }] }, "Website": { "type": "url", "url": "https://www.acme.com" } } },
                               { "id": "notacme", "properties": { "Name": { "type": "title", "title": [{ "plain_text": "Notacme" }] }, "Website": { "type": "url", "url": "https://notacme.com" } } }])
                    } else {
                        json!([])
                    };
                    (200, json!({ "results": res }).to_string())
                }
                ("POST", "/v1/pages") => {
                    let id = format!("page-{}", p2.lock().unwrap().len() + 1);
                    p2.lock().unwrap().push(id.clone());
                    (200, json!({ "id": id, "url": format!("https://www.notion.so/{id}") }).to_string())
                }
                ("GET", p) if p.starts_with("/v1/pages/") => {
                    let id = &p["/v1/pages/".len()..];
                    if p2.lock().unwrap().iter().any(|x| x == id) {
                        (200, json!({ "id": id, "archived": false }).to_string())
                    } else {
                        (404, r#"{"object":"error","status":404,"code":"object_not_found","message":"Could not find page"}"#.into())
                    }
                }
                ("PATCH", p) if p.starts_with("/v1/pages/") => {
                    let id = &p["/v1/pages/".len()..];
                    (200, json!({ "id": id, "url": format!("https://www.notion.so/{id}") }).to_string())
                }
                ("GET", p) if p.starts_with("/v1/blocks/") => (200, r#"{"results":[{"id":"old-1"},{"id":"old-2"}],"has_more":false}"#.into()),
                ("DELETE", _) => (200, "{}".into()),
                ("PATCH", p) if p.starts_with("/v1/blocks/") => {
                    let n = r.json()["children"].as_array().map(|a| a.len()).unwrap_or(0);
                    let ids: Vec<Value> = (0..n).map(|i| json!({ "id": format!("blk-{i}") })).collect();
                    (200, json!({ "results": ids }).to_string())
                }
                _ => (400, r#"{"object":"error","status":400,"code":"invalid_request_url","message":"no route"}"#.into()),
            }
        });
        (srv, pages)
    }

    fn dest(srv: &MockServer, var: &str) -> NotionDestination {
        unsafe { std::env::set_var(var, "ntn_test") };
        let mut d = DestinationConfig::new(Kind::Notion, "Notion CRM");
        d.secret_env = var.into();
        d.notion.api_base = format!("{}/v1", srv.url);
        d.notion.database_id = format!("https://www.notion.so/acme/Meetings-{}?v=1", DB.replace('-', ""));
        d.notion.tags_property = "Tags".into();
        d.notion.include_transcript = true;
        d.notion.relation = Some(RelationMatch { property: "Company".into(), ..Default::default() });
        NotionDestination::new(d)
    }

    #[test]
    fn creates_page_with_properties_relation_and_blocks() {
        let (srv, _) = fake_notion();
        let n = dest(&srv, "STENLY_TEST_NOTION_1");
        assert_eq!(n.test().unwrap(), "Connected to “Meetings”");
        let out = n.send(&testutil::payload(), None).unwrap();
        assert_eq!(out.remote_id.as_deref(), Some("page-1"));
        assert_eq!(out.url.as_deref(), Some("https://www.notion.so/page-1"));
        assert_eq!(out.detail.as_deref(), Some("Created page · linked Acme Corp"));
        let reqs = srv.requests();
        let create = reqs.iter().find(|r| r.method == "POST" && r.path == "/v1/pages").unwrap().json();
        assert_eq!(create["parent"]["database_id"], DB);
        let props = &create["properties"];
        assert_eq!(props["Name"]["title"][0]["text"]["content"], "Acme pilot");
        assert!(props["Date"]["date"]["start"].as_str().unwrap().starts_with("2026-10-20T10:00:00"));
        assert_eq!(props["Attendees"]["multi_select"].as_array().unwrap().len(), 2);
        assert_eq!(props["Tags"]["multi_select"][0]["name"], "Sales");
        assert_eq!(props["Company"]["relation"], json!([{ "id": "acme-page" }]));
        let kinds: Vec<&str> = create["children"].as_array().unwrap().iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(kinds[0], "heading_2");
        assert!(kinds.contains(&"to_do"));
        // transcript: a toggle appended to the page, then its lines into the toggle
        let appends: Vec<&Req> = reqs.iter().filter(|r| r.method == "PATCH" && r.path.starts_with("/v1/blocks/")).collect();
        assert_eq!(appends[0].path, "/v1/blocks/page-1/children");
        assert_eq!(appends[0].json()["children"][0]["type"], "toggle");
        assert_eq!(appends[1].path, "/v1/blocks/blk-0/children");
        assert_eq!(appends[1].json()["children"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn resend_updates_the_same_page() {
        let (srv, pages) = fake_notion();
        let n = dest(&srv, "STENLY_TEST_NOTION_2");
        let first = n.send(&testutil::payload(), None).unwrap();
        let again = n.send(&testutil::payload(), first.remote_id.as_deref()).unwrap();
        assert_eq!(again.remote_id, first.remote_id);
        assert!(again.detail.unwrap().starts_with("Updated page"));
        assert_eq!(pages.lock().unwrap().len(), 1, "no duplicate page");
        let reqs = srv.requests();
        assert!(reqs.iter().any(|r| r.method == "PATCH" && r.path == "/v1/pages/page-1"));
        assert_eq!(reqs.iter().filter(|r| r.method == "DELETE").count(), 2, "old blocks replaced");
        // a page deleted in Notion is recreated
        let gone = n.send(&testutil::payload(), Some("deleted-page")).unwrap();
        assert_eq!(gone.remote_id.as_deref(), Some("page-2"));
    }

    #[test]
    fn bad_token_and_missing_columns() {
        let (srv, _) = fake_notion();
        let n = dest(&srv, "STENLY_TEST_NOTION_3");
        unsafe { std::env::set_var("STENLY_TEST_NOTION_3", "wrong") };
        assert_eq!(format!("{:#}", n.test().unwrap_err()), "Notion rejected the integration token");
        unsafe { std::env::set_var("STENLY_TEST_NOTION_3", "ntn_test") };
        let mut d = n.d.clone();
        d.notion.date_property = "When".into();
        let e = format!("{:#}", NotionDestination::new(d.clone()).test().unwrap_err());
        assert!(e.contains("“When”"), "{e}");
        // missing columns are skipped on send, not fatal
        let out = NotionDestination::new(d).send(&testutil::payload(), None).unwrap();
        assert!(out.detail.unwrap().contains("skipped When"));
    }

    #[test]
    fn schema_and_suggestions() {
        let s = Schema::from_json(&meetings_db());
        assert_eq!(s.title, "Meetings");
        assert_eq!(s.columns[0], ("Name".to_string(), "title".to_string()));
        assert_eq!(s.relations, vec![("Company".to_string(), COMPANIES.to_string())]);
        let mut o = NotionOpts::default();
        suggest_mapping(&mut o, &s);
        assert_eq!((o.title_property.as_str(), o.date_property.as_str(), o.attendees_property.as_str(), o.tags_property.as_str()), ("Name", "Date", "Attendees", "Tags"));
        // an existing valid choice is kept
        o.tags_property = "Attendees".into();
        suggest_mapping(&mut o, &s);
        assert_eq!(o.tags_property, "Attendees");
    }

    #[test]
    fn rich_text_columns_get_names() {
        let db = json!({ "properties": { "T": { "type": "title" }, "Who": { "type": "rich_text" } } });
        let mut d = DestinationConfig::new(Kind::Notion, "N");
        d.notion.attendees_property = "Who".into();
        d.notion.date_property.clear();
        let n = NotionDestination::new(d);
        let mut notes = Notes::default();
        let props = n.properties(&Client::new("http://127.0.0.1:9", "x"), &db, &testutil::payload(), &mut notes).unwrap();
        assert_eq!(props["Who"]["rich_text"][0]["text"]["content"], "Ana, Bo");
        assert_eq!(props["T"]["title"][0]["text"]["content"], "Acme pilot");
    }
}
