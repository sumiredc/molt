// expect: ref_pattern
fn name_length(pair: (String, u32)) -> usize {
    let (ref name, _) = pair;
    name.len()
}
