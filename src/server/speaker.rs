//! PC-to-phone speaker stream: the `/speaker-ws` WebSocket endpoint.
//!
//! Carries the same pairing-token gate as the mic `/ws` endpoint — anyone on
//! the LAN could otherwise eavesdrop on the PC's audio. Streams 20 ms stereo
//! f32-LE PCM frames (7680 bytes each) from the shared broadcast channel; any
//! number of phones may listen at once.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::sync::broadcast;
use tracing::{info, warn};

use super::state::AppState;
use crate::speaker;

#[derive(Deserialize)]
pub(super) struct SpeakerQuery {
    token: Option<String>,
}

/// GET /speaker-ws?token=... — WebSocket upgrade for the speaker stream.
pub(super) async fn handle_speaker_ws(
    ws: WebSocketUpgrade,
    Query(query): Query<SpeakerQuery>,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Response {
    let provided_token = match query.token {
        Some(t) => t,
        None => {
            return (StatusCode::UNAUTHORIZED, "Missing token").into_response();
        }
    };

    {
        let guard = state.stream.session_token.lock();
        match guard.as_ref() {
            Some(expected)
                if super::constant_time_eq(expected.as_bytes(), provided_token.as_bytes()) => {}
            _ => {
                warn!("Speaker WebSocket rejected: invalid token");
                return (StatusCode::UNAUTHORIZED, "Invalid token").into_response();
            }
        }
    }

    let tx = match state.speaker_tx.clone() {
        Some(tx) => tx,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Speaker capture is not running on this PC",
            )
                .into_response();
        }
    };

    info!(peer = %peer, "Speaker client connected");
    let peer_ip = peer.ip().to_string();
    let peers = state.stream.speaker_peers.clone();
    peers.lock().push(peer_ip.clone());
    ws.on_upgrade(move |socket| pump(socket, tx, peer, peer_ip, peers))
}

/// Forward broadcast PCM frames to one phone until it disconnects or errors.
async fn pump(
    mut socket: WebSocket,
    tx: broadcast::Sender<Vec<f32>>,
    peer: SocketAddr,
    peer_ip: String,
    peers: Arc<parking_lot::Mutex<Vec<String>>>,
) {
    let mut rx = tx.subscribe();
    let mut sent: u64 = 0;
    loop {
        let frame = match rx.recv().await {
            Ok(f) => f,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                info!("speaker client too slow, skipped {n} frames");
                continue;
            }
            Err(_) => break,
        };
        debug_assert_eq!(frame.len(), speaker::FRAME_SAMPLES * speaker::CHANNELS);
        let mut bytes = Vec::with_capacity(speaker::FRAME_BYTES);
        for s in &frame {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        debug_assert_eq!(bytes.len(), speaker::FRAME_BYTES);
        if socket.send(Message::Binary(bytes.into())).await.is_err() {
            break;
        }
        sent += 1;
        if sent == 1 {
            info!(peer = %peer, "Speaker client streaming");
        }
    }
    peers.lock().retain(|p| p != &peer_ip);
    info!(peer = %peer, speaker_frames = sent, "Speaker client disconnected");
}
