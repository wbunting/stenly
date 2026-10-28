fn main() {
    // Tailwind scans src/ and web/ for classes and builds the stylesheet.
    hearth::tailwind::BuildConfig::new().input("src/styles.css").render().unwrap();
}
