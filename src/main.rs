#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> eframe::Result {
    if std::env::args().nth(1).as_deref() == Some("--mcp") {
        let result = (|| -> anyhow::Result<()> {
            let port = std::env::args()
                .nth(2)
                .ok_or_else(|| anyhow::anyhow!("Missing port"))?
                .parse()?;
            let key = std::env::var("YANDEX_MUSIC_MCP_KEY")?;
            yandex_music_rust::mcp::stdio(port, &key)
        })();
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }
    yandex_music_rust::ui::run()
}
