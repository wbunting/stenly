fn main() {
    // The licence gate exists only in official Developer ID release builds
    // (scripts/bundle-macos.sh sets STENLY_ENFORCE_LICENSE=1 for them). Source
    // builds never enforce anything.
    println!("cargo:rerun-if-env-changed=STENLY_ENFORCE_LICENSE");
    println!("cargo:rustc-check-cfg=cfg(stenly_enforce_license)");
    if std::env::var("STENLY_ENFORCE_LICENSE").as_deref() == Ok("1") {
        println!("cargo:rustc-cfg=stenly_enforce_license");
    }
}
