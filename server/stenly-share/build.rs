fn main() {
    // Tailwind scans src/ and web/ for classes and builds the stylesheet.
    topcoat::tailwind::BuildConfig::new().input("src/styles.css").render().unwrap();
}
