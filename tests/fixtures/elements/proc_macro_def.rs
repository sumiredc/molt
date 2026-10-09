// expect: proc_macro_def
#[proc_macro]
pub fn identity(input: TokenStream) -> TokenStream {
    input
}
