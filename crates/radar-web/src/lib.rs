//! Presentation-only WASM client. Scientific computation and SRTM access are
//! intentionally absent from this crate.
pub fn configured_api_url() -> String {
    option_env!("RADAR_API_URL")
        .unwrap_or("http://localhost:8080")
        .to_owned()
}
