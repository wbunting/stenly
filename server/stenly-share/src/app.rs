//! Router, security headers, and the HTML pages (viewer, home, health).

pub mod admin;
pub mod signin;
pub mod v1;

use hearth::{
    Result,
    asset::{Asset, AssetBundle, RouterBuilderAssetExt, asset},
    context::{Cx, app_context},
    cookie::RouterBuilderCookieExt,
    font::{Font, font},
    router::{
        Body, HeaderValue, Next, RouterBuilder, RouterBuilderDiscoverExt, StatusCode, TrustedProxies, header, module_router, page, path_param,
        response::Response, route,
    },
    runtime::RouterBuilderRuntimeExt,
    session::{RouterBuilderSessionExt, SessionConfig},
    view::ViewExt,
    view::{Unescaped, View, view},
};

use crate::{api::Bucket, auth, config::Config, model::State, share, store};

pub const STYLES: Asset = hearth::tailwind::stylesheet!();
pub const VIEWER_JS: Asset = asset!("web/viewer.js", rename: "viewer");
pub const FAVICON: Asset = asset!("assets/favicon.svg");
/// Admin page helpers (local times), loaded only on `/admin` and `/account`.
pub const ADMIN_JS: Asset = asset!("web/admin.js", rename: "admin");

pub const DM_SANS: Font = font! {
    "DM Sans",
    @font-face {
        src: url(asset!("assets/fonts/dm-sans.woff2")) format("woff2");
        font-weight: 100 900;
        font-display: swap;
    }
};
pub const INSTRUMENT_SERIF: Font = font! {
    "Instrument Serif",
    @font-face {
        src: url(asset!("assets/fonts/instrument-serif.woff2")) format("woff2");
        font-weight: 400;
        font-display: swap;
    }
};

pub fn router(cfg: &Config, assets: AssetBundle) -> RouterBuilder {
    let mut b = module_router!()
        .discover()
        .assets(assets)
        .cookies()
        .sessions(SessionConfig::builder().lifetime(auth::SESSION_LIFETIME).build())
        .layer(hearth::router::LayerFn::new(None::<&str>, security_headers))
        // The hearth runtime powers the admin's live tables.
        .runtime();
    if !cfg.trusted_proxies.is_empty() {
        b = b.trusted_proxies(TrustedProxies::new().networks(cfg.trusted_proxies.clone()));
    }
    b
}

/// Strict CSP: only same-origin scripts, styles, fonts and fetches. No inline
/// script, no eval, no third parties. `no-referrer` keeps share URLs out of
/// Referer headers (the key is in the fragment, which is never sent, but the
/// id needn't leak either).
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; font-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
/// Hearth serves link-time compiled expressions as same-origin modules and
/// toggles visibility through CSSOM: no eval or inline-style exceptions.
/// Admin and account forms post to the same origin, like sign-in pages.
const ADMIN_CSP: &str = AUTH_CSP;
/// Sign-in pages: like the viewer, plus a same-origin form post (logout).
const AUTH_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; font-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

fn security_headers<'a>(cx: &'a Cx, body: Body, next: Next<'a>) -> hearth::router::LayerFuture<'a> {
    Box::pin(async move {
        let mut res = match next.run(cx, body).await {
            Ok(r) => r,
            Err(e) => hearth::router::response::IntoResponse::into_response(e, cx)?,
        };
        let h = res.headers_mut();
        h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
        h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
        h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        h.insert(header::HeaderName::from_static("cross-origin-opener-policy"), HeaderValue::from_static("same-origin"));
        h.insert(
            header::HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static("camera=(), microphone=(), geolocation=(), interest-cohort=()"),
        );
        let html = h.get(header::CONTENT_TYPE).is_some_and(|v| v.as_bytes().starts_with(b"text/html"));
        if html {
            let path = hearth::router::request::uri(cx).path();
            let csp = if path.starts_with("/admin") || path.starts_with("/account") {
                ADMIN_CSP
            } else if path.starts_with("/auth") {
                AUTH_CSP
            } else {
                CSP
            };
            h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(csp));
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        }
        Ok(res)
    })
}

#[route(GET "/healthz")]
async fn healthz(cx: &Cx) -> Result<Response> {
    // Touch the database so a broken volume fails the health check.
    let ok = store::ping(&mut hearth::db(cx)).await.is_ok();
    let mut res = Response::new(Body::from(if ok { "ok" } else { "database unavailable" }));
    *res.status_mut() = if ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    Ok(res)
}

// ---------------------------------------------------------------- pages

/// Shared `<head>` + page chrome (masthead and colophon).
#[hearth::view::component]
pub async fn shell(cx: &Cx, title: String, description: String, #[default] child: hearth::view::Child<'_>) -> Result<impl View> {
    // Link previews only ever see the generic title or the sharer's opt-in
    // plaintext title; never note contents.
    let _ = cx;
    Ok(view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width, initial-scale=1">
                <meta name="referrer" content="no-referrer">
                <meta name="robots" content="noindex, nofollow">
                <meta name="color-scheme" content="light">
                <meta name="theme-color" content="#f4f2e9">
                <title>(&title)</title>
                <meta name="description" content=(&description)>
                <meta property="og:type" content="article">
                <meta property="og:site_name" content="Stenly">
                <meta property="og:title" content=(&title)>
                <meta property="og:description" content=(&description)>
                <meta name="twitter:card" content="summary">
                <meta name="twitter:title" content=(&title)>
                <link rel="icon" type="image/svg+xml" href=(FAVICON)>
                hearth::font::link(font: DM_SANS)
                hearth::font::link(font: INSTRUMENT_SERIF)
                <link rel="stylesheet" href=(STYLES)>
                hearth::dev::script()
            </head>
            <body>
                <div class="page">
                    <header class="masthead">
                        <a class="wordmark" href="https://stenly.ai/?ref=share" aria-label="Stenly">
                            (logo_svg())
                            <span>"stenly"</span><span class="wordmark-period">"."</span>
                        </a>
                        <span class="masthead-caption">"Private meeting notes · end-to-end encrypted"</span>
                    </header>
                    <main id="main">(child)</main>
                    <footer class="colophon">
                        <span>"Notes by Stenly · free & open source · " <a href="https://stenly.ai/?ref=share">"Get Stenly"</a></span>
                        <span class="colophon-lock">"Decrypted in your browser. This server only stores ciphertext."</span>
                    </footer>
                </div>
            </body>
        </html>
    })
}

pub fn logo_svg() -> hearth::view::Unescaped<&'static str> {
    // Static, trusted markup: the waveform mark from the app icon.
    hearth::view::Unescaped::new_unchecked(
        r#"<svg viewBox="0 0 40 40" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" aria-hidden="true"><path d="M8 18v4m6-11v18m6-24v30m6-22v14m6-9v4"/></svg>"#,
    )
}

path_param!(id);

/// The viewer. The server renders only the shell and the share's state;
/// decryption and rendering happen in `web/viewer.js`.
#[page("/s/{id}")]
async fn viewer(cx: &Cx) -> Result<impl View> {
    let id = path_param::<Id>(cx).to_owned();
    if let Err(e) = crate::api::rate_limit(cx, Bucket::Read) {
        return Ok(state_page(cx, e.status, "rate_limited", "Too many requests", "Wait a minute and reload the page."));
    }
    // Company mode: only signed-in users from allowed domains may open
    // shares. The key stays in the fragment; the sign-in page's script
    // re-attaches it after the IdP round trip (see web/viewer.js).
    if app_context::<Config>(cx).company_mode() && auth::user(cx).await?.is_none() {
        return Ok(sign_in_required(cx, &format!("/s/{id}")));
    }
    let share = if share::valid_id(&id) { store::find(&mut hearth::db(cx), &id).await? } else { None };
    let Some(share) = share else {
        return Ok(state_page(
            cx,
            StatusCode::NOT_FOUND,
            "not_found",
            "No note at this address",
            "This link doesn’t exist on this server. Check that you copied it correctly.",
        ));
    };
    match share.state(share::now()) {
        State::Revoked => {
            return Ok(state_page(
                cx,
                StatusCode::GONE,
                "revoked",
                "This note was unshared",
                "Its owner revoked the link, and the encrypted copy has been deleted from this server.",
            ));
        }
        State::Expired => {
            return Ok(state_page(
                cx,
                StatusCode::GONE,
                "expired",
                "This link has expired",
                "Shared notes are deleted after their expiry date. Ask the sender to share it again.",
            ));
        }
        State::Active => {}
    }
    let title = share.title.clone().map_or_else(|| "Meeting notes · Stenly".to_owned(), |t| format!("{t} · Stenly"));
    Ok(view! {
        shell(title: title, description: "Private meeting notes shared from Stenly, decrypted in your browser.".to_owned(),
            <div id="note" class="note-root" data-id=(&id) data-state="loading" aria-live="polite">
                <section class="state">
                    <p class="loading">(logo_svg()) "Decrypting in your browser…"</p>
                    <noscript><p class="state-body">"This note is end-to-end encrypted. Enable JavaScript to decrypt it in your browser."</p></noscript>
                </section>
            </div>
            <script type="module" src=(VIEWER_JS)></script>
        )
    }
    .boxed())
}

pub fn state_page(cx: &Cx, status: StatusCode, code: &'static str, heading: &'static str, body: &'static str) -> hearth::view::BoxView<'static> {
    view! { cx =>
        (status)
        shell(title: format!("{heading} · Stenly"), description: "Private meeting notes shared from Stenly.".to_owned(),
            <div id="note" class="note-root" data-state=(code)>
                <section class="state" role="status">
                    <h1 class="state-title">(heading)</h1>
                    <p class="state-body">(body)</p>
                </section>
            </div>
        )
    }
    .boxed()
}

#[page]
async fn home(cx: &Cx) -> Result<impl View> {
    let company = app_context::<Config>(cx).company_mode();
    let me = auth::user(cx).await?;
    Ok(view! {
        shell(title: "Stenly share".to_owned(), description: "End-to-end encrypted share links for Stenly meeting notes.".to_owned(),
            <section class="state">
                <h1 class="state-title">"Private share links for Stenly notes"</h1>
                <p class="state-body">
                    "This server stores end-to-end encrypted meeting notes shared from the Stenly app. "
                    "Notes are encrypted on the sender’s computer; the key travels only in the link, after the #, "
                    "and never reaches this server."
                </p>
                <p class="state-body"><a href="https://stenly.ai/?ref=share">"Get Stenly"</a> " · " <a href="/api/v1/info">"Server info"</a></p>
                if company {
                    if let Some(u) = &me {
                        <p class="state-body">"Signed in as " (&u.email) " · " <a href="/account">"Upload tokens"</a>
                            if u.admin { " · " <a href="/admin">"Admin"</a> }</p>
                    } else {
                        <p><a class="btn" href="/auth/sign-in">"Sign in"</a></p>
                    }
                }
            </section>
        )
    })
}

/// Where to send someone who must sign in first.
pub fn sign_in_url(return_to: &str) -> String {
    format!("/auth/sign-in?return_to={}", crate::share::urlencode(return_to))
}

/// The viewer's "sign in first" state (company mode). `viewer.js` stashes
/// the fragment key in sessionStorage before leaving, and restores it on
/// return, since IdP redirects drop fragments.
fn sign_in_required(cx: &Cx, return_to: &str) -> hearth::view::BoxView<'static> {
    let href = sign_in_url(return_to);
    view! { cx =>
        (StatusCode::UNAUTHORIZED)
        shell(title: "Sign in to view · Stenly".to_owned(), description: "Private meeting notes shared from Stenly.".to_owned(),
            <div id="note" class="note-root" data-state="sign_in" data-sign-in=(&href)>
                <section class="state" role="status">
                    <h1 class="state-title">"Sign in to view this note"</h1>
                    <p class="state-body">"This server shares notes only with signed-in people from your company."</p>
                    <p><a class="btn" id="sign-in-link" href=(&href)>"Sign in"</a></p>
                </section>
            </div>
            <script type="module" src=(VIEWER_JS)></script>
        )
    }
    .boxed()
}

/// `<script type="application/json">` with `<`, `>` and `&` escaped, so the
/// payload can't end the element. Inert under the strict CSP.
pub fn json_script(id: &str, value: &impl serde::Serialize) -> Unescaped<String> {
    let json = serde_json::to_string(value).expect("serializable");
    let json = json.replace('<', "\\u003c").replace('>', "\\u003e").replace('&', "\\u0026");
    assert!(id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'), "bad script id");
    Unescaped::new_unchecked(format!(r#"<script type="application/json" id="{id}">{json}</script>"#))
}
