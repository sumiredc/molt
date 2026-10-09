// expect: macro_rules
macro_rules! square {
    ($e:expr) => {
        $e * $e
    };
}
