//! Run a mock Google OAuth + Calendar server for driving Stenly without a Google
//! account (synthetic data):
//!
//! ```sh
//! cargo run -p stenly-core --example mock-google
//! # prints:  STENLY_GOOGLE_MOCK=http://127.0.0.1:<port>
//! STENLY_GOOGLE_CLIENT_ID=mock-client-id.apps.example.com \
//! STENLY_GOOGLE_CLIENT_SECRET=mock-client-secret \
//! STENLY_GOOGLE_MOCK=http://127.0.0.1:<port> \
//! STENLY_CONFIG=/tmp/stenly-test/config.toml STENLY_DATA_DIR=/tmp/stenly-test/data \
//!   cargo run -p stenly
//! ```
//!
//! With `STENLY_GOOGLE_MOCK_AUTO=1` the app also "clicks" the consent page itself.
#[path = "../tests/support/mock_google.rs"]
mod mock_google;

fn main() {
    let mock = mock_google::Mock::start();
    mock.seed(chrono::Utc::now());
    println!("STENLY_GOOGLE_MOCK={}", mock.base);
    println!("STENLY_GOOGLE_CLIENT_ID={}", mock_google::CLIENT_ID);
    println!("STENLY_GOOGLE_CLIENT_SECRET={}", mock_google::CLIENT_SECRET);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
