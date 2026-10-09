// expect: box_dyn_error, box, dyn_trait, result
fn run() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}
