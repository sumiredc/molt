// expect: const_generic
struct Grid<const N: usize> {
    cells: [u8; N],
}
