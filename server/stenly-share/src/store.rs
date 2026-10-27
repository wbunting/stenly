//! Database access: open + migrate, share CRUD, and the expiry sweeper.

use std::time::Duration;

use toasty::Db;

use crate::{
    config::Config,
    model::{Share, ShareBlob},
    share::{DAY, now},
};

/// Embedded, versioned schema (`migrations/`). Applied on every start;
/// already-applied migrations are skipped, so upgrades are just a restart.
static MIGRATIONS: toasty::migration::MigrationSet = toasty::embed_migrations!("migrations");

pub async fn open(cfg: &Config) -> toasty::Result<Db> {
    std::fs::create_dir_all(&cfg.data_dir).map_err(|e| toasty::Error::from_args(format_args!("creating data dir {}: {e}", cfg.data_dir.display())))?;
    open_url(&cfg.database_url()).await
}

pub async fn open_url(url: &str) -> toasty::Result<Db> {
    let mut db = Db::builder()
        .models(toasty::models!(
            crate::model::Share,
            crate::model::ShareBlob,
            crate::model::User,
            crate::model::Session,
            crate::model::UploadToken,
            crate::model::Setting,
            crate::model::ViewDay
        ))
        .connect(url)
        .await?;
    if !url.ends_with(":memory:") {
        // WAL lets the sweeper and request handlers write concurrently and
        // makes `sqlite3 .backup` safe while the server runs. Persistent.
        toasty::sql::query("PRAGMA journal_mode=WAL").exec(&mut db).await?;
    }
    MIGRATIONS.apply(&db).await?;
    Ok(db)
}

pub async fn find(db: &mut Db, id: &str) -> toasty::Result<Option<Share>> {
    Share::filter_by_id(id.to_owned()).first().exec(db).await
}

pub async fn blob(db: &mut Db, id: &str) -> toasty::Result<Option<Vec<u8>>> {
    Ok(ShareBlob::filter_by_share_id(id.to_owned()).first().exec(db).await?.map(|b| b.ciphertext))
}

pub struct NewShare<'a> {
    pub id: &'a str,
    pub owner_hash: String,
    pub title: Option<String>,
    pub envelope: Vec<u8>,
    pub expires_at: i64,
    pub owner_id: Option<String>,
}

pub async fn create(db: &mut Db, s: NewShare<'_>) -> toasty::Result<Share> {
    let t = now();
    let mut tx = db.transaction().await?;
    let share = toasty::create!(Share {
        id: s.id.to_owned(),
        owner_hash: s.owner_hash,
        title: s.title,
        size: s.envelope.len() as i64,
        created_at: t,
        updated_at: t,
        expires_at: s.expires_at,
        revoked_at: None::<i64>,
        views: 0_i64,
        purged: false,
        owner_id: s.owner_id,
    })
    .exec(&mut tx)
    .await?;
    toasty::create!(ShareBlob { share_id: s.id.to_owned(), ciphertext: s.envelope }).exec(&mut tx).await?;
    tx.commit().await?;
    Ok(share)
}

/// Replaces the envelope (re-share). The owner token never changes.
///
/// The update is guarded on the share still being active *at write time*:
/// a revoke or expiry that commits while the new body is being read wins,
/// and `None` is returned (410) instead of writing ciphertext back.
pub async fn replace(db: &mut Db, id: &str, envelope: Vec<u8>, title: Option<String>, expires_at: i64) -> toasty::Result<Option<Share>> {
    let t = now();
    let mut tx = db.transaction().await?;
    let updated = toasty::sql::statement(
        "UPDATE shares SET title = ?1, size = ?2, updated_at = ?3, expires_at = ?4 \
         WHERE id = ?5 AND revoked_at IS NULL AND purged = 0 AND expires_at > ?3",
    )
    .bind_typed(title, toasty::schema::db::Type::Text)
    .bind(envelope.len() as i64)
    .bind(t)
    .bind(expires_at)
    .bind(id.to_owned())
    .exec(&mut tx)
    .await?;
    if updated != 1 {
        tx.rollback().await?;
        return Ok(None);
    }
    ShareBlob::filter_by_share_id(id.to_owned()).delete().exec(&mut tx).await?;
    toasty::create!(ShareBlob { share_id: id.to_owned(), ciphertext: envelope }).exec(&mut tx).await?;
    let share = Share::filter_by_id(id.to_owned()).first().exec(&mut tx).await?;
    tx.commit().await?;
    Ok(share)
}

/// Revokes a share and drops its ciphertext immediately. Idempotent.
pub async fn revoke(db: &mut Db, mut share: Share) -> toasty::Result<()> {
    let mut tx = db.transaction().await?;
    if share.revoked_at.is_none() {
        share.update().revoked_at(Some(now())).purged(true).exec(&mut tx).await?;
    }
    ShareBlob::filter_by_share_id(share.id.clone()).delete().exec(&mut tx).await?;
    tx.commit().await
}

/// Runs a query whose columns are all integers (aggregates), returning one
/// `Vec<i64>` per row; SQL `NULL` (e.g. `SUM` of nothing) becomes 0.
pub async fn query_i64(db: &mut Db, sql: &str, binds: &[i64], cols: usize) -> toasty::Result<Vec<Vec<i64>>> {
    use toasty::stmt::{Type, Value};
    let mut q = toasty::sql::query(sql).column_types(std::iter::repeat_n(Type::I64, cols));
    for b in binds {
        q = q.bind(*b);
    }
    let rows = q.exec(db).await?;
    Ok(rows
        .into_iter()
        .map(|row| match row {
            Value::Record(r) => r.fields.iter().map(|v| if let Value::I64(n) = v { *n } else { 0 }).collect(),
            _ => vec![0; cols],
        })
        .collect())
}

/// Touches the database (health check).
pub async fn ping(db: &mut Db) -> toasty::Result<()> {
    toasty::sql::query("SELECT 1").exec(db).await.map(|_| ())
}

/// Counts a ciphertext fetch on the share and in the per-day stats.
pub async fn count_view(db: &mut Db, id: &str) -> toasty::Result<()> {
    toasty::sql::statement("UPDATE shares SET views = views + 1 WHERE id = ?1").bind(id.to_owned()).exec(db).await?;
    toasty::sql::statement("INSERT INTO view_days (day, views) VALUES (?1, 1) ON CONFLICT(day) DO UPDATE SET views = views + 1")
        .bind(now().div_euclid(DAY))
        .exec(db)
        .await?;
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Ciphertexts deleted (expired or revoked shares).
    pub purged: usize,
    /// Metadata rows deleted after the tombstone period.
    pub forgotten: usize,
}

/// Deletes ciphertext of expired/revoked shares, then forgets shares whose
/// tombstone period is over (they turn from 410 into 404). Also removes any
/// ciphertext whose share is purged or gone, so a blob can never outlive its
/// share.
pub async fn sweep(db: &mut Db, cfg: &Config, now: i64) -> toasty::Result<SweepReport> {
    const DEAD: &str = "purged = 0 AND (expires_at <= ?1 OR revoked_at IS NOT NULL)";
    const OLD: &str = "purged = 1 AND (expires_at <= ?1 OR revoked_at <= ?1)";
    let mut tx = db.transaction().await?;
    toasty::sql::statement(format!("DELETE FROM share_blobs WHERE share_id IN (SELECT id FROM shares WHERE {DEAD})")).bind(now).exec(&mut tx).await?;
    let purged = toasty::sql::statement(format!("UPDATE shares SET purged = 1 WHERE {DEAD}")).bind(now).exec(&mut tx).await?;
    let cutoff = now - cfg.tombstone_days * DAY;
    let forgotten = toasty::sql::statement(format!("DELETE FROM shares WHERE {OLD}")).bind(cutoff).exec(&mut tx).await?;
    // Expired sign-in sessions (company mode).
    toasty::sql::statement("DELETE FROM sessions WHERE expires_at < ?1").bind(now).exec(&mut tx).await?;
    // Backstop: no ciphertext for purged or deleted shares.
    toasty::sql::statement("DELETE FROM share_blobs WHERE share_id NOT IN (SELECT id FROM shares WHERE purged = 0)").exec(&mut tx).await?;
    tx.commit().await?;
    Ok(SweepReport { purged: purged as usize, forgotten: forgotten as usize })
}

/// Runs [`sweep`] forever on `cfg.sweep_secs`.
pub fn spawn_sweeper(db: Db, cfg: Config, limiter: std::sync::Arc<crate::api::RateLimiter>) {
    tokio::spawn(async move {
        let mut db = db;
        let mut tick = tokio::time::interval(Duration::from_secs(cfg.sweep_secs.max(1)));
        loop {
            tick.tick().await;
            match sweep(&mut db, &cfg, now()).await {
                Ok(r) if r != SweepReport::default() => eprintln!("stenly-share: sweep purged {} ciphertexts, forgot {} shares", r.purged, r.forgotten),
                Ok(_) => {}
                Err(e) => eprintln!("stenly-share: sweep failed: {e}"),
            }
            limiter.prune();
        }
    });
}
