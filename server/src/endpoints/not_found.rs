use web::{WebContext, WebFuture};

pub fn not_found(_ctx: &WebContext) -> WebFuture {
    panic!("shit");
}
