// The basic std macros.
fn main() {
    let names = vec!["a", "b"];
    println!("{} names", names.len());
    eprintln!("warning");
    let line = format!("{:?}", names);
    assert_eq!(line.is_empty(), false);
    assert!(names.len() == 2, "two names");
}
