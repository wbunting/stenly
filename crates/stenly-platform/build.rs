fn main() {
    println!("cargo:rustc-check-cfg=cfg(stenly_sparkle)");
    println!("cargo:rerun-if-env-changed=STENLY_SPARKLE_FRAMEWORK_DIR");
    println!("cargo:rerun-if-changed=src/macos/updater.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let Some(dir) = std::env::var_os("STENLY_SPARKLE_FRAMEWORK_DIR").map(std::path::PathBuf::from)
    else {
        return;
    };
    assert!(
        dir.is_absolute(),
        "Sparkle framework directory must be absolute"
    );
    assert!(
        dir.join("Sparkle.framework/Headers/SPUUpdater.h").is_file(),
        "Sparkle 2 headers missing"
    );
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let object = out.join("stenly-updater.o");
    let target = std::env::var("TARGET").unwrap();
    let native_target = match target.as_str() {
        "aarch64-apple-darwin" => "arm64-apple-macos14.2",
        "x86_64-apple-darwin" => "x86_64-apple-macos14.2",
        _ => panic!("Unsupported macOS updater target: {target}"),
    };
    let status = std::process::Command::new("xcrun")
        .args([
            "clang",
            "-target",
            native_target,
            "-fobjc-arc",
            "-fmodules",
            "-c",
            "src/macos/updater.m",
            "-o",
        ])
        .arg(&object)
        .arg("-F")
        .arg(&dir)
        .status()
        .expect("run clang");
    assert!(status.success(), "Sparkle adapter compilation failed");
    let status = std::process::Command::new("ar")
        .arg("crs")
        .arg(out.join("libstenly_updater.a"))
        .arg(&object)
        .status()
        .expect("run ar");
    assert!(status.success(), "Sparkle adapter archive failed");
    println!("cargo:rustc-cfg=stenly_sparkle");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-search=framework={}", dir.display());
    println!("cargo:rustc-link-lib=static=stenly_updater");
    println!("cargo:rustc-link-lib=framework=Sparkle");
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
}
