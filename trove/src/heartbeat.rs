use std::sync::Arc;

use futures::Future;
use tokio;
use tokio::executor::current_thread;
use tokio_timer::Timer;

use types::TroveState;

use smith_aorta::{HeartbeatResponse, QueryStatus};

pub(crate) fn spawn_heartbeat(state: Arc<TroveState>) {
    info!("starting heartbeat service");
    schedule_heartbeat(state);
}

fn schedule_heartbeat(state: Arc<TroveState>) {
    let inner_state = state.clone();
    let config = &state.config();
    let timer = Timer::default();
    tokio::spawn(
        timer
            .sleep(config.heartbeat_interval.to_std().unwrap())
            .and_then(|_| {
                perform_heartbeat(inner_state);
                Ok(())
            })
            .or_else(|_| -> Result<_, _> {
                panic!("failed to schedule register");
            }),
    );
}

fn perform_heartbeat(state: Arc<TroveState>) {
    // if we encounter a non authenticated state we shut down.  The authenticator
    // code will respawn us.
    if !state.auth_state().is_authenticated() {
        warn!("heartbeat service encountered non authenticated trove. shutting down");
        return;
    }

    let hb_req = state.query_manager().next_heartbeat_request();
    let inner_state_success = state.clone();
    let inner_state_failure = state.clone();

    current_thread::spawn(
        state
            .aorta_request(&hb_req)
            .and_then(move |response| {
                handle_heartbeat_response(inner_state_success.clone(), response);
                schedule_heartbeat(inner_state_success);
                Ok(())
            })
            .or_else(|err| {
                error!("heartbeat failed: {}", &err);
                schedule_heartbeat(inner_state_failure);
                Err(())
            }),
    );
}

fn handle_heartbeat_response(state: Arc<TroveState>, response: HeartbeatResponse) {
    let query_manager = state.query_manager();
    for (query_id, result) in response.query_results.into_iter() {
        if result.status == QueryStatus::Pending {
            continue;
        }
        if let Some((project_id, mut callback)) = query_manager.pop_callback(query_id) {
            if let Some(project_state) = state.get_project_state(project_id) {
                if let Some(data) = result.result {
                    callback(&project_state, data, result.status == QueryStatus::Success);
                }
            }
        }
    }
}
