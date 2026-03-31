extern crate proc_macro;
use proc_macro::TokenStream;

#[proc_macro_derive(MyDerive)]
pub fn my_derive(input: TokenStream) -> TokenStream {
    TokenStream::new()
}

#[proc_macro_attribute]
pub fn my_attribute(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

#[proc_macro]
pub fn my_function_like(input: TokenStream) -> TokenStream {
    input
}

#[proc_macro_derive(WithHelper, attributes(helper))]
pub fn with_helper(input: TokenStream) -> TokenStream {
    TokenStream::new()
}
