use std::sync::Arc;

use serde::ser::Serialize;
use serde_json;
use failure;
use futures::future::{self, Future};
use hyper;
use hyper::StatusCode;
use hyper::header::{ContentLength, ContentType};
use hyper::server::{Request, Response};

use smith_trove::TroveState;

/// Return value from
pub struct WebResponse {
    resp: Response,
}

pub type WebFuture = Box<Future<Item = WebResponse, Error = WebError>>;
pub type WebError = failure::Error;

/// Api requests are passed to endpoints.
///
/// They give access to a trove context as well as the web request
/// that sits underneath it as well as various helpers.
pub struct WebContext {
    request: Request,
    trove_state: Arc<TroveState>,
}

impl WebContext {
    /// Creates a new api request based on a hyper web request and a trove state.
    pub fn new(request: Request, trove_state: Arc<TroveState>) -> WebContext {
        WebContext {
            request: request,
            trove_state: trove_state,
        }
    }

    /// Returns an immutable reference to the underlying web request.
    pub fn request(&self) -> &Request {
        &self.request
    }

    /// Returns the trove state.
    pub fn trove_state(&self) -> Arc<TroveState> {
        self.trove_state.clone()
    }
}

impl WebResponse {
    /// Creates a new json response with a given status code.
    pub fn with_json_and_status<S: Serialize>(status_code: StatusCode, s: &S) -> WebResponse {
        let mut resp = Response::new();
        let body = serde_json::to_vec(s).unwrap();
        {
            resp.headers_mut().set(ContentType::json());
            resp.headers_mut().set(ContentLength(body.len() as u64));
        }
        resp.set_body(body);
        WebResponse { resp: resp }
    }

    /// Creates a new JSON reponse with 200 ok.
    pub fn with_json<S: Serialize>(s: &S) -> WebResponse {
        WebResponse::with_json_and_status(StatusCode::Ok, s)
    }

    /// Converts the response into a hyper response
    pub fn into_hyper_response(self) -> Response {
        self.resp
    }
}
