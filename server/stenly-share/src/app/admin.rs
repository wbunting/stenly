//! `/admin`: live list of shares (hearth synced table), revoke, extend
//! expiry, server settings and stats. Admins only. Never shows or derives
//! note contents: only the metadata in `ShareRow`.

use hearth::{live_query, table_handle};
use hearth::{
    Result,
    context::{Cx, app_context},
    cookie::{Cookie, Cookies, SameSite, cookies, time},
    router::{
        content::Form,
        error::{SeeOther, see_other},
        page, route,
    },
    runtime::{Event, signal},
    view::{View, ViewExt, component, view},
};

use crate::{
    app::{logo_svg, sign_in_url},
    auth,
    config::Config,
    model::{UploadToken, User},
    policy::{self, PolicyCell},
    share::{self, DAY},
    store,
    tables::{ShareRow, UserRow, client_schema, require_admin},
};

/// A list row (`#[hearth::row]`: a record the browser fills from the local
/// table and `$()` reads).
#[hearth::row]
pub struct AdminShare {
    pub id: String,
    pub title: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub size: i64,
    pub views: i64,
    pub revoked: bool,
    pub owner: Option<String>,
}

#[page("/admin")]
async fn admin(cx: &Cx) -> Result<impl View> {
    let Some(me) = auth::user(cx).await? else {
        return Err(hearth::router::error::redirect(sign_in_url("/admin")).into());
    };
    if !me.admin {
        return Ok(view! {
            admin_shell(me: me.clone(), <section class="state"><h1 class="state-title">"Admins only"</h1>
                <p class="state-body">"Ask an admin of this server to add you to STENLY_SHARE_ADMINS."</p></section>)
        }
        .boxed());
    }
    let mut db = hearth::db(cx);
    let cfg = app_context::<Config>(cx).clone();
    let policy = app_context::<PolicyCell>(cx).get();
    let stats = stats(&mut db).await?;

    // Page-load time: rows expiring while the page is open keep their
    // status until reload (the server still enforces expiry).
    let now = share::now();

    // ---- browser state
    let show = signal(cx, || "active".to_owned()); // active | expired | revoked | all
    let text = signal(cx, String::new);
    let shares_t = table_handle::<ShareRow>();

    // Live query over the synced `shares` table: runs on the server for the
    // first view, then in the browser; revoke/extend update it optimistically.
    let list = live_query! {
        AdminShare from s in ShareRow
        left join u in UserRow on s.owner_id == u.id
        where (if $(show.get() == "active") { s.revoked == false && s.expires_at > $(now) })
            && (if $(show.get() == "expired") { s.revoked == false && s.expires_at <= $(now) })
            && (if $(show.get() == "revoked") { s.revoked == true })
            && if $(!text.get().is_empty()) { s.id contains $(text.get()) }
        order by s.created_at desc
        limit 500
        select { id: s.id, title: s.title, created_at: s.created_at, expires_at: s.expires_at, size: s.size, views: s.views, revoked: s.revoked, owner: u.email }
    };

    let boot = serde_json::json!({ "schema": client_schema(format!("admin:{}", me.id))
        .on_unauthorized(hearth::OnUnauthorized::Redirect(sign_in_url("/admin"))), "snapshot": {} });
    let domains = policy.allowed_email_domains.join(", ");

    Ok(view! {
        admin_shell(me: me.clone(),
            <section class="admin-stats" aria-label="Stats">
                stat(label: "Active shares", value: stats.active.to_string())
                stat(label: "Shares (all time)", value: stats.total.to_string())
                stat(label: "Views", value: stats.views.to_string())
                stat(label: "Stored", value: human_bytes(stats.bytes))
                stat(label: "Users", value: stats.users.to_string())
            </section>

            <section class="admin-panel">
                <h2 class="admin-h2">"Last 14 days"</h2>
                (spark(&stats.created_days, &stats.view_days))
            </section>

            <section class="admin-panel">
                <div class="admin-toolbar">
                    <h2 class="admin-h2">"Shares"</h2>
                    <select class="admin-input" aria-label="Filter" @change=$(|e: Event| show.set(e.target.value))>
                        <option value="active" selected="">"Active"</option>
                        <option value="expired">"Expired"</option>
                        <option value="revoked">"Revoked"</option>
                        <option value="all">"All"</option>
                    </select>
                    <input class="admin-input" type="search" placeholder="Filter by id…" aria-label="Filter by id" @input=$(|e: Event| text.set(e.target.value))>
                </div>
                <p class="admin-note">"Titles appear only when the sharer opted in to a plaintext title for link previews. Note contents are encrypted and never visible here."</p>
                <div class="admin-table-wrap">
                    <table class="admin-table">
                        <thead><tr>
                            <th>"Share"</th><th>"Owner"</th><th>"Created"</th><th>"Expires"</th>
                            <th class="num">"Size"</th><th class="num">"Views"</th><th>"Status"</th><th></th>
                        </tr></thead>
                        <tbody>
                            for s in $(list) {
                                <tr :class=$(if s.revoked { "is-revoked" } else if s.expires_at <= now { "is-revoked" } else { "" })>
                                    <td>
                                        <span class="share-title">$(s.title.clone().unwrap_or("—".to_owned()))</span>
                                        <code class="share-id">$(s.id.clone())</code>
                                    </td>
                                    <td class="muted">$(s.owner.clone().unwrap_or("—".to_owned()))</td>
                                    <td><time class="ts" :data-ts=$(s.created_at.to_text())>$(s.created_at.to_text())</time></td>
                                    <td><time class="ts" :data-ts=$(s.expires_at.to_text())>$(s.expires_at.to_text())</time></td>
                                    <td class="num">$(if s.size < 1024i64 { s.size.to_text() + " B" } else { (s.size / 1024i64).to_text() + " KB" })</td>
                                    <td class="num">$(s.views)</td>
                                    <td>$(if s.revoked { "Revoked" } else if s.expires_at <= now { "Expired" } else { "Active" })</td>
                                    <td class="actions-cell">
                                        if $(if s.revoked { false } else { s.expires_at > now }) {
                                            <button class="btn btn-quiet btn-sm" title="Extend expiry by 30 days"
                                                @click=$(|_e| shares_t.set(s.id.clone(), "expiresAt", s.expires_at + 2592000i64))>"+30 days"</button>
                                            <button class="btn btn-danger btn-sm" title="Revoke: deletes the ciphertext"
                                                @click=$(|_e| shares_t.set(s.id.clone(), "revoked", true))>"Revoke"</button>
                                        }
                                    </td>
                                </tr>
                            }
                        </tbody>
                    </table>
                </div>
            </section>

            <section class="admin-panel">
                <h2 class="admin-h2">"Server settings"</h2>
                <form class="admin-form" method="post" action="/admin/settings">
                    <label>"Default expiry (days)" <input class="admin-input" name="default_ttl_days" type="number" min="1" max=(cfg.max_ttl_days.to_string()) value=(policy.default_ttl_days.to_string()) required=""></label>
                    <label>"Max note size (KiB)" <input class="admin-input" name="max_kib" type="number" min="1" max="65536" value=((policy.max_bytes / 1024).to_string()) required=""></label>
                    <label class="wide">"Allowed email domains (comma-separated; empty = anyone your identity provider signs in)"
                        <input class="admin-input" name="allowed_email_domains" type="text" value=(domains) placeholder="example.com"></label>
                    <div class="wide"><button class="btn">"Save settings"</button></div>
                </form>
                <p class="admin-note">"Environment: max expiry " (cfg.max_ttl_days.to_string()) " days · upload auth: "
                    (if cfg.upload_token.is_some() { "shared token or personal tokens" } else if cfg.company_mode() && cfg.upload_auth == crate::config::UploadAuth::User { "personal tokens only" } else { "open" })</p>
            </section>
            (crate::app::json_script("hearth-boot", &boot))
        )
    }
    .boxed())
}

#[component]
async fn stat(label: &'static str, value: String) -> Result<impl View> {
    Ok(view! { <div class="stat"><div class="stat-value">(value)</div><div class="stat-label">(label)</div></div> })
}

/// Admin layout: same masthead, a nav, and the hearth runtime.
#[component]
pub async fn admin_shell(me: User, #[default] child: hearth::view::Child<'_>) -> Result<impl View> {
    Ok(view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width, initial-scale=1">
                <meta name="robots" content="noindex, nofollow">
                <title>"Admin · Stenly share"</title>
                <link rel="icon" type="image/svg+xml" href=(crate::app::FAVICON)>
                hearth::font::link(font: crate::app::DM_SANS)
                hearth::font::link(font: crate::app::INSTRUMENT_SERIF)
                <link rel="stylesheet" href=(crate::app::STYLES)>
                hearth::runtime::script()
                <script type="module" src=(crate::app::ADMIN_JS)></script>
                hearth::dev::script()
            </head>
            <body>
                <div class="page page-wide">
                    <header class="masthead">
                        <a class="wordmark" href="/admin" aria-label="Stenly share admin">(logo_svg())<span>"stenly"</span><span class="wordmark-period">"."</span></a>
                        <nav class="admin-nav">
                            <a href="/admin">"Shares"</a>
                            <a href="/account">"Upload tokens"</a>
                            <span class="muted">(&me.email)</span>
                            <form method="post" action="/auth/logout"><button class="linkish">"Sign out"</button></form>
                        </nav>
                    </header>
                    <main id="main">(child)</main>
                </div>
            </body>
        </html>
    })
}

#[derive(Debug, serde::Deserialize)]
struct SettingsForm {
    default_ttl_days: i64,
    max_kib: usize,
    #[serde(default)]
    allowed_email_domains: String,
}

#[route(POST "/admin/settings")]
async fn save_settings(cx: &Cx, Form(f): Form<SettingsForm>) -> Result<SeeOther> {
    require_admin(cx).await?;
    let domains = policy::parse_domains(&f.allowed_email_domains).map_err(hearth::router::error::bad_request)?;
    let new = policy::Policy { default_ttl_days: f.default_ttl_days, max_bytes: f.max_kib.saturating_mul(1024), allowed_email_domains: domains };
    let cfg = app_context::<Config>(cx);
    policy::save(&mut hearth::db(cx), cfg, app_context::<PolicyCell>(cx), new).await.map_err(hearth::router::error::bad_request)?;
    Ok(see_other("/admin"))
}

// ---------------------------------------------------------------- stats

pub struct Stats {
    pub total: i64,
    pub active: i64,
    pub users: i64,
    pub views: i64,
    pub bytes: i64,
    /// (day, shares created) for the last 14 days.
    pub created_days: Vec<(i64, i64)>,
    pub view_days: Vec<(i64, i64)>,
}

pub async fn stats(db: &mut toasty::Db) -> Result<Stats> {
    let now = share::now();
    let today = now.div_euclid(DAY);
    let first = today - 13;
    let one = |rows: Vec<Vec<i64>>| rows.into_iter().next().unwrap_or_default();
    let all = one(store::query_i64(db, "SELECT COUNT(*), COALESCE(SUM(views), 0) FROM shares", &[], 2).await?);
    let active =
        one(store::query_i64(db, "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM shares WHERE revoked_at IS NULL AND expires_at > ?1", &[now], 2).await?);
    let users = one(store::query_i64(db, "SELECT COUNT(*) FROM users", &[], 1).await?);
    let mut created = vec![0i64; 14];
    for r in store::query_i64(db, &format!("SELECT created_at / {DAY} AS d, COUNT(*) FROM shares WHERE created_at >= ?1 GROUP BY d"), &[first * DAY], 2).await?
    {
        if let Some(slot) = usize::try_from(r[0] - first).ok().and_then(|i| created.get_mut(i)) {
            *slot = r[1];
        }
    }
    let mut viewed = vec![0i64; 14];
    for r in store::query_i64(db, "SELECT day, views FROM view_days WHERE day >= ?1", &[first], 2).await? {
        if let Some(slot) = usize::try_from(r[0] - first).ok().and_then(|i| viewed.get_mut(i)) {
            *slot = r[1];
        }
    }
    let at = |v: &[i64], i: usize| v.get(i).copied().unwrap_or(0);
    Ok(Stats {
        total: at(&all, 0),
        active: at(&active, 0),
        views: at(&all, 1),
        bytes: at(&active, 1),
        users: at(&users, 0),
        created_days: created.into_iter().enumerate().map(|(i, n)| (first + i as i64, n)).collect(),
        view_days: viewed.into_iter().enumerate().map(|(i, n)| (first + i as i64, n)).collect(),
    })
}

fn human_bytes(n: i64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{:.2} MB", n as f64 / 1_048_576.0),
    }
}

/// Two-series bar chart (shares created, views) as static SVG.
fn spark(created: &[(i64, i64)], views: &[(i64, i64)]) -> hearth::view::Unescaped<String> {
    let max = created.iter().chain(views).map(|(_, n)| *n).max().unwrap_or(0).max(1) as f64;
    let (w, h, bw) = (560.0, 96.0, 40.0);
    let mut s = format!(r#"<svg class="spark" viewBox="0 0 {w} {}" role="img" aria-label="Shares created and views per day, last 14 days">"#, h + 18.0);
    for (i, ((day, c), (_, v))) in created.iter().zip(views).enumerate() {
        let x = i as f64 * bw;
        let ch = (*c as f64 / max * h).round();
        let vh = (*v as f64 / max * h).round();
        let date = ::time::OffsetDateTime::from_unix_timestamp(day * DAY).map(|d| format!("{}/{}", u8::from(d.month()), d.day())).unwrap_or_default();
        s.push_str(&format!(
            r##"<g><title>{date}: {c} shares, {v} views</title><rect x="{:.0}" y="{:.0}" width="14" height="{ch:.0}" fill="#25251f"/><rect x="{:.0}" y="{:.0}" width="14" height="{vh:.0}" fill="#b5c03f"/><text x="{:.0}" y="{:.0}" font-size="9" text-anchor="middle" fill="#605f55">{date}</text></g>"##,
            x + 4.0,
            h - ch,
            x + 20.0,
            h - vh,
            x + 20.0,
            h + 13.0
        ));
    }
    s.push_str(r##"</svg><p class="admin-note"><span class="key key-ink"></span>shares created <span class="key key-lime"></span>views</p>"##);
    hearth::view::Unescaped::new_unchecked(s)
}

// ---------------------------------------------------------------- personal upload tokens

#[page("/account")]
async fn account(cx: &Cx) -> Result<impl View> {
    let Some(me) = auth::user(cx).await? else {
        return Err(hearth::router::error::redirect(sign_in_url("/account")).into());
    };
    let mut tokens = UploadToken::filter_by_user_id(me.id.clone()).exec(&mut hearth::db(cx)).await?;
    tokens.sort_by_key(|t| -t.created_at);
    let jar = cookies(cx);
    let fresh = jar.get("new_upload_token").map(|c| c.value().to_owned());
    if fresh.is_some() {
        jar.remove(Cookie::build(("new_upload_token", "")).path("/account").build());
    }
    let base = crate::api::base_url(cx, app_context::<Config>(cx));
    Ok(view! {
        admin_shell(me: me.clone(),
            <section class="admin-panel">
                <h2 class="admin-h2">"Upload tokens"</h2>
                <p class="admin-note">"Stenly needs a token to share notes on this server. In Stenly: Settings → Destinations → Stenly share link, set the server URL to "
                    <code>(&base)</code> " and paste a token as its credential."</p>
                if let Some(t) = &fresh {
                    <div class="token-once"><p>"Your new token. Copy it now; it won’t be shown again."</p><code>(t)</code></div>
                }
                <form class="admin-form" method="post" action="/account/tokens">
                    <label class="wide">"Label" <input class="admin-input" name="label" maxlength="60" placeholder="MacBook" required=""></label>
                    <div class="wide"><button class="btn">"Create token"</button></div>
                </form>
                <table class="admin-table">
                    <thead><tr><th>"Label"</th><th>"Created"</th><th>"Last used"</th><th></th></tr></thead>
                    <tbody>
                        for t in &tokens {
                            <tr>
                                <td>(&t.label)</td>
                                <td><time class="ts" data-ts=(t.created_at.to_string())>(t.created_at.to_string())</time></td>
                                <td>if let Some(u) = t.last_used_at { <time class="ts" data-ts=(u.to_string())>(u.to_string())</time> } else { "never" }</td>
                                <td class="actions-cell"><form method="post" action="/account/tokens/delete"><input type="hidden" name="id" value=(&t.id)><button class="btn btn-danger btn-sm">"Delete"</button></form></td>
                            </tr>
                        }
                    </tbody>
                </table>
            </section>
        )
    })
}

#[derive(Debug, serde::Deserialize)]
struct NewToken {
    label: String,
}

#[route(POST "/account/tokens")]
async fn create_token(cx: &Cx, Form(f): Form<NewToken>) -> Result<SeeOther> {
    let me = auth::user(cx).await?.ok_or_else(hearth::router::error::forbidden)?;
    let label: String = f.label.chars().filter(|c| !c.is_control()).take(60).collect::<String>().trim().to_owned();
    if label.is_empty() {
        return Err(hearth::router::error::bad_request("label is required").into());
    }
    let token = format!("ssu_{}", share::new_owner_token());
    toasty::create!(UploadToken {
        id: share::new_id(),
        token_hash: share::token_hash(&token),
        user_id: me.id,
        label,
        created_at: share::now(),
        last_used_at: None::<i64>
    })
    .exec(&mut hearth::db(cx))
    .await?;
    // Shown once on the next page (short-lived, HttpOnly, path-scoped).
    cookies(cx).add(
        Cookie::build(("new_upload_token", token))
            .path("/account")
            .http_only(true)
            .secure(true)
            .same_site(SameSite::Strict)
            .max_age(time::Duration::seconds(120))
            .build(),
    );
    Ok(see_other("/account"))
}

#[derive(Debug, serde::Deserialize)]
struct DeleteToken {
    id: String,
}

#[route(POST "/account/tokens/delete")]
async fn delete_token(cx: &Cx, Form(f): Form<DeleteToken>) -> Result<SeeOther> {
    let me = auth::user(cx).await?.ok_or_else(hearth::router::error::forbidden)?;
    let mut db = hearth::db(cx);
    if let Some(t) = UploadToken::filter_by_id(f.id).first().exec(&mut db).await?
        && t.user_id == me.id
    {
        UploadToken::filter_by_id(t.id).delete().exec(&mut db).await?;
    }
    Ok(see_other("/account"))
}
