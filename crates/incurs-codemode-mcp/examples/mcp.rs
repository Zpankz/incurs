use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), String> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "codemode.sqlite".into());
    let service = incurs_codemode_local::spawn_codemode(path, Default::default(), vec![])?;
    incurs_codemode_mcp::serve_stdio(Arc::new(service)).await
}
