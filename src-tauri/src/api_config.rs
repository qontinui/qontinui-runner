/// Get API base URL for qontinui-web backend.
///
/// Resolution order:
/// 1. `QONTINUI_API_URL` environment variable (if set)
/// 2. Debug builds: `http://127.0.0.1:8000` (IPv4 — backend only binds IPv4,
///    localhost may resolve to IPv6 `::1` first)
/// 3. Release builds: production backend URL
pub fn get_api_base_url() -> String {
    std::env::var("QONTINUI_API_URL").unwrap_or_else(|_| {
        if cfg!(debug_assertions) {
            "http://127.0.0.1:8000".to_string()
        } else {
            "https://qontinui-prod-py.eba-km2u4s23.eu-central-1.elasticbeanstalk.com".to_string()
        }
    })
}
