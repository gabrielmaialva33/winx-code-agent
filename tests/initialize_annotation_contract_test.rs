//! Public tool hints must describe the most consequential supported operation.
use rmcp::ServerHandler;
use winx_code_agent::WinxService;

#[test]
fn initialize_does_not_hide_reset_under_a_read_only_hint() -> anyhow::Result<()> {
    let service = WinxService::new();
    let tool = service
        .get_tool("Initialize")
        .ok_or_else(|| anyhow::anyhow!("Initialize missing from catalog"))?;
    let hints = tool.annotations.ok_or_else(|| anyhow::anyhow!("missing annotations"))?;
    assert_ne!(hints.read_only_hint, Some(true), "reset_shell can replace a live shell");
    assert_eq!(hints.destructive_hint, Some(true));
    Ok(())
}

#[test]
fn observation_tools_keep_read_only_hints() -> anyhow::Result<()> {
    let service = WinxService::new();
    for name in ["ReadFiles", "ReadImage", "CodeMap", "Execution"] {
        let tool =
            service.get_tool(name).ok_or_else(|| anyhow::anyhow!("{name} missing from catalog"))?;
        let hints =
            tool.annotations.ok_or_else(|| anyhow::anyhow!("missing {name} annotations"))?;
        assert_eq!(hints.read_only_hint, Some(true), "{name}");
    }
    Ok(())
}
