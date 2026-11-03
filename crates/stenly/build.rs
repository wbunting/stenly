fn main() {
    println!("cargo:rerun-if-env-changed=STENLY_SPARKLE_FRAMEWORK_DIR");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
        && std::env::var_os("STENLY_SPARKLE_FRAMEWORK_DIR").is_some()
    {
        // Link args from a library build script do not propagate to this binary.
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
    }
    // STENLY_RELEASE_REPOSITORY is validated and compiled in by stenly-core
    // (crates/stenly-core/build.rs); read it with stenly_core::update::release_repository().
}
