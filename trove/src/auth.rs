use std::sync::Arc;

use futures::Future;
use tokio;
use tokio_timer::Timer;

use types::TroveState;

use smith_aorta::{RegisterChallenge, RegisterRequest};

/// Represents the current auth state of the trove.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum AuthState {
    Unknown,
    RegisterRequestChallenge,
    RegisterChallengeResponse,
    Registered,
    Error,
}

impl AuthState {
    /// Returns true if the state is considered authenticated
    pub fn is_authenticated(&self) -> bool {
        // XXX: the goal of auth state is that it also tracks auth
        // failures from the heartbeat.  Later we will need to
        // extend the states here for it.
        *self == AuthState::Registered
    }
}

#[derive(Fail, Debug)]
#[fail(display = "could not authenticate")]
pub struct AuthError;

pub(crate) fn spawn_authenticator(state: Arc<TroveState>) {
    state.set_auth_state(AuthState::Unknown);
    debug!("starting authenticator");
    register_with_upstream(state);
}

fn register_with_upstream(state: Arc<TroveState>) {
    let config = &state.config();

    info!(
        "registering with upstream (upstream = {})",
        &config.upstream
    );

    state.set_auth_state(AuthState::RegisterRequestChallenge);

    let inner_state_success = state.clone();
    let inner_state_failure = state.clone();
    let reg_req = RegisterRequest::new(config.relay_id(), config.public_key());
    tokio::spawn(
        state
            .aorta_request(&reg_req)
            .and_then(|challenge| {
                info!("got register challenge (token = {})", challenge.token());
                send_register_challenge_response(inner_state_success, challenge);
                Ok(())
            })
            .or_else(|err| {
                // XXX: do not schedule retries for fatal errors
                error!("authentication encountered error: {}", &err);
                schedule_auth_retry(inner_state_failure);
                Err(())
            }),
    );
}

fn send_register_challenge_response(state: Arc<TroveState>, challenge: RegisterChallenge) {
    info!("sending register challenge response");

    state.set_auth_state(AuthState::RegisterChallengeResponse);

    let inner_state_success = state.clone();
    let inner_state_failure = state.clone();
    let challenge_resp_req = challenge.create_response();
    tokio::spawn(
        state
            .aorta_request(&challenge_resp_req)
            .and_then(move |_| {
                info!("relay successfully registered with upstream");
                inner_state_success.set_auth_state(AuthState::Registered);
                Ok(())
            })
            .or_else(|err| {
                // XXX: do not schedule retries for fatal errors
                error!("failed to register relay with upstream: {}", &err);
                schedule_auth_retry(inner_state_failure);
                Err(())
            }),
    );
}

fn schedule_auth_retry(state: Arc<TroveState>) {
    info!("scheduling authentication retry");
    let config = &state.config();
    state.set_auth_state(AuthState::Error);
    let timer = Timer::default();
    let inner_state = state.clone();
    tokio::spawn(
        timer
            .sleep(config.auth_retry_interval.to_std().unwrap())
            .and_then(|_| {
                register_with_upstream(inner_state);
                Ok(())
            })
            .or_else(|_| -> Result<_, _> {
                panic!("failed to schedule register");
            }),
    );
}
