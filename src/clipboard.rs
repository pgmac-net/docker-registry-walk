pub fn copy_to_clipboard(text: &str) -> anyhow::Result<()> {
    let mut ctx = arboard::Clipboard::new()?;
    ctx.set_text(text)?;
    Ok(())
}
