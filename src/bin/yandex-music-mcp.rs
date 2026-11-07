fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let port = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing MCP port"))?
        .parse()?;
    let token = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing MCP key"))?;
    yandex_music_rust::mcp::stdio(port, &token)
}
