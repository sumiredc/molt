// expect: result
fn length(text: String) -> Result<u32, String> {
    Ok(text.len() as u32)
}
