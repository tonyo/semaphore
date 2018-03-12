use std::io;
use std::thread;
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde_json;
use parking_lot::{Mutex, RwLock};
use tokio;
use tokio_core::reactor::{Core, Handle};
use futures::{Future, IntoFuture, Stream};
use futures::future;
use futures::sync::{mpsc, oneshot};
use hyper;
use hyper::{Chunk, Request, StatusCode};
use hyper::header::ContentType;
use hyper::client::{Client, HttpConnector};
use hyper_tls::HttpsConnector;

use smith_common::ProjectId;
use smith_aorta::{AortaConfig, ApiErrorResponse, ApiRequest, ProjectState, QueryManager};

use auth::{spawn_authenticator, AuthError, AuthState};
use heartbeat::spawn_heartbeat;

thread_local! {
    static CORE_AND_HANDLE: (RefCell<Core>, Handle) = {
        let core = RefCell::new(Core::new().unwrap());
        let handle = core.borrow().handle();
        (core, handle)
    };
    static HTTP_CLIENT: Arc<Client<HttpsConnector<HttpConnector>>> = {
        CORE_AND_HANDLE.with(|&(_, ref handle)| {
            Arc::new(Client::configure()
                .connector(HttpsConnector::new(4, &handle).unwrap())
                .build(&handle))
        })
    }
}

/// Represents an event that can be sent to the governor.
#[derive(Debug)]
pub(crate) enum GovernorEvent {
    /// Tells the trove governor to shut down.
    Shutdown,
    /// The trove authenticated successfully.
    Authenticated,
}

/// Raised for errors that happen in the context of trove governing.
#[derive(Debug, Fail)]
pub enum GovernorError {
    /// Raised if the governor could not be spawned.
    #[fail(display = "could not spawn governor thread")]
    SpawnError(#[cause] io::Error),
    /// Raised if the event loop failed to spawn.
    #[fail(display = "cannot spawn event loop")]
    CannotSpawnEventLoop(#[cause] io::Error),
    /// Raised if the authentication handler could not spawn.
    #[fail(display = "governor could not start authentication")]
    CannotSpawnAuthenticator(#[cause] AuthError),
    /// Raised if the governor panicked.
    #[fail(display = "governor thread panicked")]
    Panic,
}

/// Raised for API Errors
#[derive(Debug, Fail)]
pub enum ApiError {
    /// On deserialization errors.
    #[fail(display = "could not deserialize response")]
    BadPayload(#[cause] serde_json::Error),
    /// On general http errors.
    #[fail(display = "http error")]
    HttpError(#[cause] hyper::Error),
    /// A server indicated api error ocurred.
    #[fail(display = "bad request ({}; {})", _0, _1)]
    ErrorResponse(StatusCode, ApiErrorResponse),
}

unsafe impl Send for ApiError {}

/// An internal helper struct that represents the shared state of the
/// trove.  An `Arc` of the state is passed around various systems.
#[derive(Debug)]
pub struct TroveState {
    config: Arc<AortaConfig>,
    states: RwLock<HashMap<ProjectId, Arc<ProjectState>>>,
    governor_tx: RwLock<Option<mpsc::UnboundedSender<GovernorEvent>>>,
    is_governed: AtomicBool,
    query_manager: Arc<QueryManager>,
    auth_state: RwLock<AuthState>,
}

/// The trove holds project states and manages the upstream aorta.
///
/// The trove can be used from multiple threads as the object is internally
/// synchronized however the typical case is to work with the the trove
/// state or context.  It also typically has a governing thread running which
/// automatically manages the state for the individual projects.
pub struct Trove {
    state: Arc<TroveState>,
    join_handle: Mutex<Option<thread::JoinHandle<Result<(), GovernorError>>>>,
}

impl Trove {
    /// Creates a new empty trove for an upstream.
    ///
    /// The config must already be stored in an Arc so it can be effectively
    /// shared around.
    pub fn new(config: Arc<AortaConfig>) -> Trove {
        Trove {
            state: Arc::new(TroveState {
                states: RwLock::new(HashMap::new()),
                config: config,
                governor_tx: RwLock::new(None),
                is_governed: AtomicBool::new(false),
                query_manager: Arc::new(QueryManager::new()),
                auth_state: RwLock::new(AuthState::Unknown),
            }),
            join_handle: Mutex::new(None),
        }
    }

    /// Returns the internal state of the trove.
    pub fn state(&self) -> Arc<TroveState> {
        self.state.clone()
    }

    /// Spawns a trove governor thread.
    ///
    /// The trove governor is a controlling background thread that manages the
    /// state of the trove.  If the trove is already governed this function will
    /// panic.
    pub fn govern(&self) -> Result<(), GovernorError> {
        if self.state.is_governed() {
            panic!("trove is already governed");
        }

        let state = self.state.clone();
        debug!("spawning trove governor");

        *self.join_handle.lock() = Some(thread::Builder::new()
            .name("trove-governor".into())
            .spawn(move || {
                let (tx, rx) = mpsc::unbounded::<GovernorEvent>();
                *state.governor_tx.write() = Some(tx);
                run_governor(state, rx)
            })
            .map_err(GovernorError::SpawnError)?);

        Ok(())
    }

    /// Abdicates the governor.
    ///
    /// Unlike governing it's permissible to call this method multiple times.
    /// If the trove is already not governed this method will just quietly
    /// do nothing.  Additionally dropping the trove will attempt to abdicate
    /// and kill the thread.  This however might not be executed as there is no
    /// guarantee that dtors are invoked in all cases .
    pub fn abdicate(&self) -> Result<(), GovernorError> {
        if !self.state.is_governed() {
            return Ok(());
        }

        // indicate on the trove state that we're no longer governed and then
        // attempt to send a message into the event channel if we can safely
        // do so.
        self.state.emit_governor_event(GovernorEvent::Shutdown);

        if let Some(handle) = self.join_handle.lock().take() {
            match handle.join() {
                Err(_) => Err(GovernorError::Panic),
                Ok(result) => result,
            }
        } else {
            Ok(())
        }
    }
}

impl Drop for Trove {
    fn drop(&mut self) {
        self.abdicate().unwrap();
    }
}

impl TroveState {
    /// Returns `true` if the trove is governed.
    pub fn is_governed(&self) -> bool {
        self.is_governed.load(Ordering::Relaxed)
    }

    /// Returns true if the trove is healthy.
    pub fn is_healthy(&self) -> bool {
        self.is_governed() && self.auth_state().is_authenticated()
    }

    /// Returns the aorta config.
    pub fn config(&self) -> Arc<AortaConfig> {
        self.config.clone()
    }

    /// Returns the current auth state.
    pub fn auth_state(&self) -> AuthState {
        *self.auth_state.read()
    }

    /// Returns a project state if it exists.
    pub fn get_project_state(&self, project_id: ProjectId) -> Option<Arc<ProjectState>> {
        self.states.read().get(&project_id).map(|x| x.clone())
    }

    /// Gets or creates the project state.
    pub fn get_or_create_project_state(&self, project_id: ProjectId) -> Arc<ProjectState> {
        // state already exists, return it.
        {
            let states = self.states.read();
            if let Some(ref rv) = states.get(&project_id) {
                return (*rv).clone();
            }
        }

        // insert an empty state
        {
            let state =
                ProjectState::new(project_id, self.config.clone(), self.query_manager.clone());
            self.states.write().insert(project_id, Arc::new(state));
        }
        (*self.states.read().get(&project_id).unwrap()).clone()
    }

    /// Returns the current query manager.
    pub fn query_manager(&self) -> Arc<QueryManager> {
        self.query_manager.clone()
    }

    /// Transitions the auth state.
    pub fn set_auth_state(&self, new_state: AuthState) {
        let mut auth_state = self.auth_state.write();
        let old_state = *auth_state;
        if old_state != new_state {
            info!("changing auth state: {:?} -> {:?}", old_state, new_state);
            *auth_state = new_state;
        }

        // if we transition from non authenticated to authenticated
        // we emit an event.
        if !old_state.is_authenticated() && new_state.is_authenticated() {
            self.emit_governor_event(GovernorEvent::Authenticated);
        }
    }

    /// Emits an event to the governor.
    pub(crate) fn emit_governor_event(&self, event: GovernorEvent) -> bool {
        info!("emit governor event: {:?}", event);
        if let Some(ref tx) = *self.governor_tx.read() {
            tx.unbounded_send(event).is_ok()
        } else {
            false
        }
    }

    /// Returns a reference to the http client.
    pub fn http_client(&self) -> Arc<Client<HttpsConnector<HttpConnector>>> {
        HTTP_CLIENT.with(|client| client.clone())
    }

    fn perform_aorta_request<D: DeserializeOwned + Send + 'static>(
        &self,
        req: Request,
    ) -> Box<Future<Item = D, Error = ApiError> + Send> {
        Box::new(
            self.http_client()
                .request(req)
                .map_err(ApiError::HttpError)
                .and_then(|res| -> Box<Future<Item = D, Error = ApiError> + Send> {
                    let status = res.status();
                    if status.is_success() {
                        Box::new(res.body().concat2().map_err(ApiError::HttpError).and_then(
                            move |body: Chunk| {
                                Ok(serde_json::from_slice::<D>(&body)
                                    .map_err(ApiError::BadPayload)?)
                            },
                        ))
                    } else if res.headers()
                        .get::<ContentType>()
                        .map(|x| x.type_() == "application" && x.subtype() == "json")
                        .unwrap_or(false)
                    {
                        // error response
                        Box::new(res.body().concat2().map_err(ApiError::HttpError).and_then(
                            move |body: Chunk| {
                                let err: ApiErrorResponse =
                                    serde_json::from_slice(&body).map_err(ApiError::BadPayload)?;
                                Err(ApiError::ErrorResponse(status, err))
                            },
                        ))
                    } else {
                        Box::new(
                            Err(ApiError::ErrorResponse(status, Default::default())).into_future(),
                        )
                    }
                }),
        )
    }

    /// Shortcut for aorta requests for specific types.
    pub fn aorta_request<T: ApiRequest>(
        &self,
        req: &T,
    ) -> Box<Future<Item = T::Response, Error = ApiError> + Send> {
        let (method, path) = req.get_aorta_request_target();
        let req = self.config.prepare_aorta_req(method, &path, req);
        self.perform_aorta_request::<T::Response>(req)
    }
}

fn run_governor(
    state: Arc<TroveState>,
    rx: mpsc::UnboundedReceiver<GovernorEvent>,
) -> Result<(), GovernorError> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let mut shutdown_tx = Some(shutdown_tx);

    state.is_governed.store(true, Ordering::Relaxed);
    debug!("spawned trove governor");

    let block_state = state.clone();
    CORE_AND_HANDLE.with(|&(ref core, _)| {
        core.borrow_mut()
            .run(future::lazy(move || -> Result<_, GovernorError> {
                let loop_state = block_state.clone();
                debug!("spawning event handler loop");
                tokio::spawn(rx.for_each(move |event| {
                    match event {
                        GovernorEvent::Shutdown => {
                            if let Some(shutdown_tx) = shutdown_tx.take() {
                                shutdown_tx.send(()).ok();
                            }
                        }
                        GovernorEvent::Authenticated => {
                            // XXX: temporarily disabled
                            //spawn_heartbeat(loop_state.clone());
                        }
                    }
                    Ok(())
                }));
                spawn_authenticator(block_state);
                tokio::spawn(shutdown_rx.map_err(|_| ()));
                Ok(())
            }))
    })?;

    state.is_governed.store(false, Ordering::Relaxed);
    debug!("shut down trove governor");

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_assert_sync() {
        struct Assert<T: Sync> {
            x: Option<T>,
        }
        let val: Assert<Trove> = Assert { x: None };
        assert_eq!(val.x.is_none(), true);
    }
}
