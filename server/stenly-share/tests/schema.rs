//! The latest committed Toasty snapshot (`migrations/snapshots/`) must match
//! the models, so `toasty migration generate` can diff the next schema change
//! against it. After changing a model, add a migration (never edit an
//! existing snapshot): `toasty migration generate` writes the next
//! `NNNN_*.sql` + snapshot and its `history.toml` entry. The test only
//! writes a snapshot that doesn't exist yet (`STENLY_SHARE_WRITE_SNAPSHOT=1`,
//! for a history entry added by hand).

use stenly_share::store;
use toasty::migration::Snapshot;

#[tokio::test]
async fn snapshot_matches_models() {
    let db = store::open_url("sqlite::memory:").await.unwrap();
    let current = Snapshot::new(db.schema().db.clone()).to_toml_string().unwrap();
    let history = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations/history.toml")).unwrap();
    let name = history.lines().rev().find_map(|l| l.strip_prefix("snapshot_name = ")).expect("snapshot_name in history.toml").trim_matches('"').to_owned();
    let path = format!("{}/migrations/snapshots/{name}", env!("CARGO_MANIFEST_DIR"));
    if std::env::var_os("STENLY_SHARE_WRITE_SNAPSHOT").is_some() && !std::path::Path::new(&path).exists() {
        std::fs::create_dir_all(format!("{}/migrations/snapshots", env!("CARGO_MANIFEST_DIR"))).unwrap();
        std::fs::write(&path, &current).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing {path} (named by the last history.toml entry)"));
    assert_eq!(
        committed, current,
        "the models changed since the last migration: run `toasty migration generate` to add a new migration + snapshot (don't edit {name})"
    );
}

/// The embedded SQL migrations produce the same tables, columns and indexes
/// as the models, so the hand-written SQL can't drift from Toasty's schema.
#[tokio::test]
async fn migrations_match_models() {
    let dir = std::env::temp_dir().join(format!("stenly-share-schema-{}", stenly_share::share::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let migrated = store::open_url(&format!("sqlite:{}", dir.join("m.db").display())).await.unwrap();
    let pushed = toasty::Db::builder()
        .models(toasty::models!(
            stenly_share::model::Share,
            stenly_share::model::ShareBlob,
            stenly_share::model::User,
            stenly_share::model::Session,
            stenly_share::model::UploadToken,
            stenly_share::model::Setting,
            stenly_share::model::ViewDay
        ))
        .connect(&format!("sqlite:{}", dir.join("p.db").display()))
        .await
        .unwrap();
    pushed.push_schema().await.unwrap();
    const TABLES: &str = "m.type = 'table' AND m.name NOT LIKE '\\_\\_%' ESCAPE '\\'";
    let shape = |mut db: toasty::Db| async move {
        let mut out = String::new();
        for sql in [
            format!(
                "SELECT m.name || '.' || p.name || ':' || p.type || ':' || p.\"notnull\" FROM sqlite_master m JOIN pragma_table_info(m.name) p WHERE {TABLES} ORDER BY 1"
            ),
            // Indexes: table, name, uniqueness and columns in order.
            format!(
                "SELECT m.name || ':' || il.name || ':' || il.\"unique\" || ':' || ii.seqno || ':' || ii.name FROM sqlite_master m \
                 JOIN pragma_index_list(m.name) il JOIN pragma_index_info(il.name) ii WHERE {TABLES} AND il.origin = 'c' ORDER BY 1"
            ),
        ] {
            let rows = toasty::sql::query(sql).exec(&mut db).await.unwrap();
            out.push_str(&format!("{rows:?}\n"));
        }
        out
    };
    let (a, b) = (shape(migrated).await, shape(pushed).await);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(a, b);
}
