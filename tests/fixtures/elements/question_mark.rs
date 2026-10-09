// expect: question_mark, result
fn doubled(text: String) -> Result<u32, std::num::ParseIntError> {
    let n: u32 = text.parse()?;
    Ok(n * 2)
}
