fn main() {
    // The licence gate exists only in official Developer ID release builds
    // (scripts/bundle-macos.sh sets STENLY_ENFORCE_LICENSE=1 for them). Source
    // builds never enforce anything.
    println!("cargo:rerun-if-env-changed=STENLY_ENFORCE_LICENSE");
    println!("cargo:rerun-if-env-changed=STENLY_POLAR_ORG_ID");
    println!("cargo:rerun-if-env-changed=STENLY_POLAR_API");
    println!("cargo:rerun-if-env-changed=STENLY_CHECKOUT_URL");
    // Pass the public Polar settings through to option_env!.
    for var in ["STENLY_POLAR_ORG_ID", "STENLY_POLAR_API", "STENLY_CHECKOUT_URL"] {
        if let Ok(v) = std::env::var(var) {
            println!("cargo:rustc-env={var}={v}");
        }
    }
    // The Google OAuth client for "Sign in with Google" (a Desktop-app client).
    // Its secret isn't confidential, but it is kept out of the repository:
    // supply both at build time (the release workflow reads them from
    // repository variables/secrets). Without an id the sign-in buttons are
    // hidden and nothing else changes.
    for var in ["STENLY_GOOGLE_CLIENT_ID", "STENLY_GOOGLE_CLIENT_SECRET"] {
        println!("cargo:rerun-if-env-changed={var}");
        if let Ok(v) = std::env::var(var)
            && !v.trim().is_empty()
        {
            println!("cargo:rustc-env={var}={}", v.trim());
        }
    }
    // GitHub Releases source for update notifications (OWNER/REPO). Official
    // release builds set it; source/dev builds deliberately have no default.
    // It also turns update checks on by default (see AppConfig).
    println!("cargo:rerun-if-env-changed=STENLY_RELEASE_REPOSITORY");
    if let Ok(repo) = std::env::var("STENLY_RELEASE_REPOSITORY")
        && !repo.is_empty()
    {
        let parts: Vec<_> = repo.split('/').collect();
        assert!(
            parts.len() == 2
                && parts.iter().all(|p| !p.is_empty()
                    && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')),
            "Expected GitHub OWNER/REPO"
        );
        println!("cargo:rustc-env=STENLY_RELEASE_REPOSITORY={repo}");
    }
    println!("cargo:rustc-check-cfg=cfg(stenly_enforce_license)");
    if std::env::var("STENLY_ENFORCE_LICENSE").as_deref() == Ok("1") {
        // A gate with no way to buy or activate would lock users out.
        assert!(std::env::var("STENLY_POLAR_ORG_ID").is_ok_and(|v| !v.trim().is_empty()), "STENLY_ENFORCE_LICENSE=1 needs STENLY_POLAR_ORG_ID");
        assert!(std::env::var("STENLY_CHECKOUT_URL").is_ok_and(|v| v.starts_with("https://")), "STENLY_ENFORCE_LICENSE=1 needs an https STENLY_CHECKOUT_URL");
        println!("cargo:rustc-cfg=stenly_enforce_license");
    }
}
