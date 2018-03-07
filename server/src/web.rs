use std::sync::Arc;

use hyper::server::Request;

use smith_trove::TroveContext;

/// Api requests are passed to endpoints.
///
/// They give access to a trove context as well as the web request
/// that sits underneath it as well as various helpers.
pub struct ApiRequest {
    web_request: Request,
    trove_ctx: Arc<TroveContext>,
}

impl ApiRequest {
    /// Creates a new api request based on a hyper web request and a trove context.
    pub fn new(web_request: Request, trove_ctx: Arc<TroveContext>) -> ApiRequest {
        ApiRequest {
            web_request: web_request,
            trove_ctx: trove_ctx,
        }
    }

    /// Returns an immutable reference to the underlying web request.
    pub fn web_request(&self) -> &Request {
        &self.web_request
    }
}
