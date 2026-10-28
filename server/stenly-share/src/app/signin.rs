//! `/auth/*`: OIDC sign-in for company mode.

use hearth::{
    Result,
    context::{Cx, app_context},
    router::{
        StatusCode,
        error::{SeeOther, see_other},
        page, query_params, route,
    },
    view::{View, ViewExt, view},
};

use crate::{
    app::{shell, state_page},
    auth::{self, PendingLogins},
    config::Config,
    oidc,
    share::urlencode,
};

fn redirect_uri(cfg: &Config) -> String {
    format!("{}/auth/callback", cfg.base_url.as_deref().unwrap_or(""))
}

#[query_params(error = bad_request)]
struct SignInQuery {
    return_to: Option<String>,
}

/// The sign-in page (company mode). Links to the IdP.
#[page("/auth/sign-in")]
async fn sign_in_page(cx: &Cx) -> Result<impl View> {
    let cfg = app_context::<Config>(cx);
    if !cfg.company_mode() {
        return Ok(state_page(cx, StatusCode::NOT_FOUND, "not_found", "Sign-in isn’t enabled", "This server shares notes without accounts."));
    }
    let q = query_params::<SignInQuery>(cx)?;
    let return_to = auth::safe_return_to(q.return_to.as_deref().unwrap_or("/"));
    let start_url = format!("/auth/start?return_to={}", urlencode(&return_to));
    Ok(view! {
        shell(title: "Sign in · Stenly share".to_owned(), description: "Sign in to view meeting notes shared in your company.".to_owned(),
            <section class="state">
                <h1 class="state-title">"Sign in to view notes"</h1>
                <p class="state-body">"Notes on this server are shared within your company. Sign in with your work account to open them."</p>
                <p><a class="btn" href=(start_url)>"Continue with your company account"</a></p>
                <p class="state-body small">"The note still stays end-to-end encrypted: signing in only lets you fetch the encrypted copy; the key in your link decrypts it in this browser."</p>
            </section>
        )
    }
    .boxed())
}

#[route(GET "/auth/start")]
async fn start(cx: &Cx) -> Result<SeeOther> {
    let cfg = app_context::<Config>(cx);
    let Some(o) = &cfg.oidc else { return Err(hearth::router::error::not_found().into()) };
    // Each start holds a pending login (and may fetch discovery): limit it.
    // The callback needs a `state` only a (limited) start can mint.
    crate::api::rate_limit(cx, crate::api::Bucket::Auth)?;
    let q = query_params::<SignInQuery>(cx)?;
    let d = auth::discovery(cx).await?;
    let (state, pending) = oidc::new_pending(&auth::safe_return_to(q.return_to.as_deref().unwrap_or("/")));
    let url = oidc::authorize_url(&d, o, &redirect_uri(cfg), &state, &pending);
    app_context::<PendingLogins>(cx).put(state, pending);
    Ok(see_other(url))
}

#[query_params(error = bad_request)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[page("/auth/callback")]
async fn callback(cx: &Cx) -> Result<impl View> {
    let cfg = app_context::<Config>(cx);
    let Some(o) = cfg.oidc.clone() else { return Err(hearth::router::error::not_found().into()) };
    let q = query_params::<Callback>(cx)?;
    let fail = |msg: String| Ok(state_page_owned(cx, StatusCode::BAD_REQUEST, "Sign-in didn’t work", msg));
    if let Some(e) = &q.error {
        return fail(format!("The identity provider said: {e}."));
    }
    let (Some(code), Some(state)) = (q.code.clone(), q.state.clone()) else { return fail("The sign-in response was incomplete. Try again.".into()) };
    let Some(pending) = app_context::<PendingLogins>(cx).take(&state) else {
        return fail("This sign-in link expired or was already used. Try again.".into());
    };
    let d = auth::discovery(cx).await?;
    let redirect = redirect_uri(cfg);
    let p2 = pending.clone();
    let identity = match tokio::task::spawn_blocking(move || oidc::exchange(&d, &o, &redirect, &code, &p2)).await {
        Ok(Ok(id)) => id,
        Ok(Err(e)) => {
            eprintln!("stenly-share: sign-in failed: {e}");
            return fail("The identity provider’s response couldn’t be verified. Try again.".into());
        }
        Err(e) => return Err(hearth::router::error::internal_server_error(e).into()),
    };
    match auth::sign_in(cx, &identity).await {
        Ok(_) => Err(hearth::router::error::see_other(pending.return_to).into()),
        Err(e) if e.code == "domain_not_allowed" => Ok(state_page_owned(
            cx,
            StatusCode::FORBIDDEN,
            "This account isn’t allowed here",
            format!("{} isn’t in a domain this server accepts. Sign in with your work account.", identity.email),
        )),
        Err(e) => Err(e.into()),
    }
}

#[route(POST "/auth/logout")]
async fn logout(cx: &Cx) -> Result<SeeOther> {
    auth::sign_out(cx).await?;
    Ok(see_other("/"))
}

fn state_page_owned(cx: &Cx, status: StatusCode, heading: &'static str, body: String) -> hearth::view::BoxView<'static> {
    view! { cx =>
        (status)
        shell(title: format!("{heading} · Stenly"), description: "Stenly share".to_owned(),
            <section class="state" role="status">
                <h1 class="state-title">(heading)</h1>
                <p class="state-body">(body)</p>
                <p><a class="btn btn-quiet" href="/auth/sign-in">"Try again"</a></p>
            </section>
        )
    }
    .boxed()
}
