fn main() {
    println!("cargo:rerun-if-env-changed=STENLY_SPARKLE_FRAMEWORK_DIR");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
        && std::env::var_os("STENLY_SPARKLE_FRAMEWORK_DIR").is_some()
    {
        // Link args from a library build script do not propagate to this binary.
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
    }
    // Deliberate release-owner input. No private or guessed default repository.
    println!("cargo:rerun-if-env-changed=STENLY_RELEASE_REPOSITORY");
    if let Ok(repo) = std::env::var("STENLY_RELEASE_REPOSITORY") {
        let parts: Vec<_> = repo.split('/').collect();
        assert!(
            parts.len() == 2
                && parts.iter().all(|p| !p.is_empty()
                    && p.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')),
            "Expected GitHub OWNER/REPO"
        );
        println!("cargo:rustc-env=STENLY_RELEASE_REPOSITORY={repo}");
    }
}
