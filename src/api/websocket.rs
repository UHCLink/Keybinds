//! Le jeton de page est envoyé dans la première trame, jamais dans l'URL.
//! Les contrôles Host et Origin du routeur s'appliquent avant l'upgrade.
use super::*;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
enum Request {
    Authenticate {
        id: u64,
        token: String,
    },
    State {
        id: u64,
        active: bool,
        recording: bool,
    },
    Config {
        id: u64,
        revision: u64,
        config: ShortcutConfig,
    },
    Pong {},
}

pub(super) async fn upgrade(
    State(state): State<Arc<LocalState>>,
    upgrade: WebSocketUpgrade,
    connection: Option<axum::Extension<transport::ConnectionPermit>>,
) -> Response {
    let Ok(permit) = state.sockets.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    upgrade
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(65_536)
        .max_message_size(4096)
        .max_frame_size(4096)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            // Conserver aussi le permis TCP après la fin de la requête HTTP.
            let _connection = connection;
            serve(state, socket).await;
        })
}

async fn send(socket: &mut WebSocket, message: Message) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(2), socket.send(message)).await,
        Ok(Ok(()))
    )
}

async fn snapshot(
    socket: &mut WebSocket,
    state: &LocalState,
    token: &str,
    after: &mut u64,
    id: Option<u64>,
    error: Option<StatusCode>,
) -> bool {
    if !state.inner.lock().unwrap().clients.contains_key(token) {
        return false;
    }
    let payload = state.snapshot(token, *after);
    *after = payload.cursor;
    let value =
        serde_json::json!({"id":id,"snapshot":payload,"error":error.map(|code|code.as_u16())});
    send(socket, Message::Text(value.to_string().into())).await
}

fn touch(state: &LocalState, token: &str) -> bool {
    let mut inner = state.inner.lock().unwrap();
    let Some(client) = inner.clients.get_mut(token) else {
        return false;
    };
    if client.seen.elapsed() >= LEASE {
        return false;
    }
    client.seen = Instant::now();
    true
}

async fn serve(state: Arc<LocalState>, mut socket: WebSocket) {
    let Ok(Some(Ok(Message::Text(text)))) =
        tokio::time::timeout(Duration::from_secs(5), socket.recv()).await
    else {
        return;
    };
    let Ok(Request::Authenticate { id, token }) = serde_json::from_str::<Request>(&text) else {
        return;
    };
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return;
    }
    {
        let mut inner = state.inner.lock().unwrap();
        let Some(client) = inner.clients.get_mut(&token) else {
            return;
        };
        if client.websocket || client.seen.elapsed() >= LEASE {
            return;
        }
        client.websocket = true;
        client.seen = Instant::now();
    }
    let mut headers = HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    let mut after = state.inner.lock().unwrap().cursor;
    if snapshot(&mut socket, &state, &token, &mut after, Some(id), None).await {
        let mut interval = tokio::time::interval(Duration::from_millis(200));
        let mut ping = Instant::now();
        let mut incoming_rate = RateLimit::new(120, 60);
        loop {
            let changed = state.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            tokio::select! {
                _ = changed => {
                    if !snapshot(&mut socket, &state, &token, &mut after, None, None).await { break; }
                }
                _ = interval.tick() => {
                    let (alive, held) = {
                        let inner = state.inner.lock().unwrap();
                        (inner.clients.get(&token).is_some_and(|client|client.seen.elapsed() < LEASE), inner.held)
                    };
                    if !alive { break; }
                    if ping.elapsed() >= Duration::from_secs(1) {
                        if !send(&mut socket, Message::Text("{\"type\":\"ping\"}".into())).await { break; }
                        ping = Instant::now();
                    }
                    // Renouvelle la fermeture de sécurité PTT sur l'horloge audio.
                    // Au repos, aucune requête HTTP ni aucun instantané périodique.
                    if held && !snapshot(&mut socket, &state, &token, &mut after, None, None).await { break; }
                }
                incoming = socket.recv() => {
                    if !incoming_rate.allow() || !touch(&state, &token) { break; }
                    match incoming {
                        Some(Ok(Message::Pong(_))) | Some(Ok(Message::Ping(_))) => {},
                        Some(Ok(Message::Text(text))) => {
                            let Ok(command) = serde_json::from_str::<Request>(&text) else { break };
                            let (id, result) = match command {
                                Request::State { id, active, recording } => (id, set_state(State(state.clone()), headers.clone(), Json(Active { active, recording })).await),
                                Request::Config { id, revision, config } => (id, set_config(State(state.clone()), headers.clone(), Json(Update { revision, config })).await),
                                Request::Authenticate { .. } => break,
                                Request::Pong {} => continue,
                            };
                            if !snapshot(&mut socket, &state, &token, &mut after, Some(id), result.err()).await { break; }
                        },
                        _ => break,
                    }
                }
            }
        }
    }
    {
        let mut inner = state.inner.lock().unwrap();
        inner.clients.remove(&token);
        inner.events.retain(|(client, _)| client != &token);
    }
    state.refresh();
    state.changed.notify_waiters();
    let _ = send(&mut socket, Message::Close(None)).await;
}
