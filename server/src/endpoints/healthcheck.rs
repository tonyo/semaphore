use futures::future;
use hyper::StatusCode;
use web::{WebContext, WebFuture, WebResponse};

#[derive(Serialize, Debug)]
pub struct Healthcheck {
    pub is_healthy: bool,
}

pub fn healthcheck(ctx: &WebContext) -> WebFuture {
    let is_healthy = ctx.trove_state().is_healthy();
    Box::new(future::ok(WebResponse::with_json_and_status(
        if is_healthy {
            StatusCode::Ok
        } else {
            StatusCode::ServiceUnavailable
        },
        &Healthcheck {
            is_healthy: is_healthy,
        },
    )))
}
