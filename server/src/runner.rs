use std::panic;
use std::sync::Arc;

use parking_lot::Mutex;
use futures::future::{self, Future};
use futures::sync::oneshot;
use hyper::{Error as HyperError, StatusCode};
use hyper::header::{ContentLength, ContentType};
use hyper::server::{Http, Request, Response, Service};
use failure::ResultExt;

use errors::{Error, ErrorKind};
use utils::make_error_response;
use web::WebContext;
use endpoints;

use smith_config::Config;
use smith_aorta::ApiErrorResponse;
use smith_trove::{Trove, TroveState};
use smith_common::ProjectId;

static TEXT: &'static str = "Doing absolutely nothing so far!";

struct RootService {
    state: Arc<TroveState>,
}

/*
lazy_static! {
    static ref ROUTER: Router = {
        let mut router = Router::new();
        router.add_route("/api/0/healthcheck/", endpoints::healthcheck);
        router
    };
}
*/

impl Service for RootService {
    type Request = Request;
    type Response = Response;
    type Error = HyperError;
    type Future = Box<Future<Item = Self::Response, Error = Self::Error>>;

    fn call(&self, req: Request) -> Self::Future {
        panic::catch_unwind(panic::AssertUnwindSafe(|| -> Self::Future {
            let web_ctx = WebContext::new(req, self.state.clone());
            let future = if web_ctx.request().path() == "/api/0/healthcheck/" {
                endpoints::healthcheck(&web_ctx)
            } else {
                endpoints::not_found(&web_ctx)
            };
            Box::new(future.map(|x| x.into_hyper_response()).or_else(|err| {
                make_error_response(
                    StatusCode::InternalServerError,
                    ApiErrorResponse::with_detail("cannot convert errors yet"),
                )
            }))
        })).unwrap_or_else(|_| {
            make_error_response(
                StatusCode::InternalServerError,
                ApiErrorResponse::with_detail(
                    "The server encountered a fatal internal server error",
                ),
            )
        })
    }
}

/// Given a relay config spawns the server and lets it run until it stops.
///
/// This not only spawning the server but also a governed trove in the
/// background.  Effectively this boots the server.
pub fn run(config: &Config, shutdown_rx: oneshot::Receiver<()>) -> Result<(), Error> {
    let shutdown_rx = shutdown_rx.shared();
    let trove = Arc::new(Trove::new(config.make_aorta_config()));
    trove.govern().context(ErrorKind::TroveGovernSpawnFailed)?;

    // the service itself has a landing pad but in addition we also create one
    // for the server entirely in case we encounter a bad panic somewhere.
    loop {
        info!("spawning http listener");

        let trove = trove.clone();
        let shutdown_rx = shutdown_rx.clone();
        if panic::catch_unwind(panic::AssertUnwindSafe(|| -> Result<(), Error> {
            let trove = trove.clone();
            let server = Http::new()
                .bind(&config.listen_addr(), move || {
                    Ok(RootService {
                        state: trove.state(),
                    })
                })
                .context(ErrorKind::BindFailed)?;
            server
                .run_until(shutdown_rx.map(|_| ()).map_err(|_| ()))
                .context(ErrorKind::ListenFailed)?;
            Ok(())
        })).is_ok()
        {
            break;
        }
        warn!("tearning down http listener for respawn because of uncontained panic");
    }

    trove.abdicate().context(ErrorKind::TroveGovernSpawnFailed)?;

    info!("relay shut down");

    Ok(())
}
