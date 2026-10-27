/// Use the bundle beside this test's profile directory (debug or release),
/// including custom Cargo target directories and compilation targets.
pub fn assets() -> topcoat::asset::AssetBundle {
    let dir = std::env::var_os("STENLY_SHARE_ASSETS_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| {
        let exe = std::env::current_exe().expect("test executable path");
        exe.parent().and_then(std::path::Path::parent).expect("test executable in <profile>/deps").join("assets")
    });
    topcoat::asset::AssetBundle::load_dir(&dir)
        .unwrap_or_else(|e| panic!("asset bundle at {} ({e}); run `topcoat asset bundle --release` for release tests first", dir.display()))
}
