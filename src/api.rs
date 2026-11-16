//! API locale de découverte HTTP et de communication WebSocket.
//!
//! `POST /connect` crée un jeton de page. `/ws` reçoit ce jeton dans la première
//! trame, puis transporte l'activation, les réglages et les actions logiques.
//! Les routes HTTP historiques restent disponibles pour les anciens navigateurs.
//! Aucune route ne transmet du son, du texte saisi ou des entrées clavier brutes.
//!
//! Plusieurs pages actives partagent les raccourcis du poste. Leur configuration
//! reste commune, une capture de touche suspend les actions pendant sa durée.
//! Les jetons, baux, actions et connexions ne sont jamais sauvegardés sur disque.
//! Host et Origin sont vérifiés avant l'upgrade, l'écoute reste en boucle locale.
//! Ces contrôles ne constituent pas une isolation contre un processus hostile
//! déjà exécuté sur le même PC, capable de fabriquer ses propres requêtes.
use crate::{Action, ShortcutConfig};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Query, State,
    },
    http::{HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    io::Read,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{Notify, Semaphore};
mod transport;
mod websocket;

/// Premier port essayé, la plage est inclusive et également définie côté site.
pub const PORT: u16 = 32145;
/// Dernier port essayé, soit 101 possibilités au total.
pub const LAST_PORT: u16 = 32245;
// Une page doit continuer à contacter le composant pour conserver les actions.
// Le minuteur de serve_with_ready réévalue aussi l'expiration sans nouvelle requête.
const LEASE: Duration = Duration::from_secs(4);
const SETTINGS_LIMIT: usize = 16_384;

// Seau à jetons borné, sans file d'attente ni tâche supplémentaire par commande.
// Les pongs WebSocket ont une limite séparée de celle des écritures sur disque.
pub(super) struct RateLimit {
    available: f64,
    updated: Instant,
    burst: f64,
    per_second: f64,
}
impl RateLimit {
    fn new(burst: u32, per_second: u32) -> Self {
        Self {
            available: burst as f64,
            updated: Instant::now(),
            burst: burst as f64,
            per_second: per_second as f64,
        }
    }
    fn allow(&mut self) -> bool {
        let now = Instant::now();
        self.available = (self.available
            + now.duration_since(self.updated).as_secs_f64() * self.per_second)
            .min(self.burst);
        self.updated = now;
        if self.available < 1.0 {
            return false;
        }
        self.available -= 1.0;
        true
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Saved {
    // Révision de configuration seulement, pas le curseur des actions en mémoire.
    revision: u64,
    config: ShortcutConfig,
}
struct Client {
    // Une page peut lire/configurer sans demander la réception des raccourcis.
    active: bool,
    recording: bool,
    websocket: bool,
    seen: Instant,
    requests: RateLimit,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
/// Action logique numérotée, copiée dans les files des pages actives.
pub struct Event {
    /// Curseur croissant, utilisé par le site pour ne pas rejouer une action.
    pub id: u64,
    /// `microphone`, `headphones` ou `ptt`, jamais le nom d'une touche physique.
    pub action: &'static str,
    /// Pour PTT: appui/relâchement. Pour une bascule: toujours `true`.
    pub pressed: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    // Version du protocole JSON, indépendante de la version du logiciel.
    protocol: u8,
    // Capacité additive permettant au site de reconnaître les anciennes versions.
    mouse_buttons: bool,
    modifier_keys: bool,
    web_socket: bool,
    multi_client: bool,
    // Distingue l'activation acceptée d'une suspension par la capture d'une autre page.
    requested_active: bool,
    version: &'static str,
    revision: u64,
    config: ShortcutConfig,
    cursor: u64,
    events: Vec<Event>,
    // Toujours fermé pour une page inactive ou pendant une capture de touche.
    held: bool,
    active: bool,
    // Le curseur de la page est plus ancien que la fenêtre conservée en mémoire.
    overflow: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    // Présent uniquement dans la réponse initiale de connexion.
    token: Option<String>,
}
#[derive(Serialize)]
struct Disconnected {
    ok: bool,
}
struct Inner {
    saved: Saved,
    clients: HashMap<String, Client>,
    // Jeton destinataire + action: deux pages ne partagent pas leur file.
    events: VecDeque<(String, Event)>,
    cursor: u64,
    held: bool,
    // Dernière paire (révision, activation native) appliquée au thread Windows.
    applied: (u64, bool),
    connections: RateLimit,
    writes: RateLimit,
}
impl Inner {
    // Toutes les pages actives reçoivent les actions, sans dépendre du focus.
    // La capture d'une touche suspend temporairement les actions du poste.
    fn active_clients(&self) -> Vec<String> {
        if self
            .clients
            .values()
            .any(|client| client.recording && client.seen.elapsed() < LEASE)
        {
            return Vec::new();
        }
        self.clients
            .iter()
            .filter(|(_, client)| client.active && client.seen.elapsed() < LEASE)
            .map(|(token, _)| token.clone())
            .collect()
    }
}
/// État partagé entre les routes HTTP, le minuteur et l'intégration native.
/// Les verrous standard ne restent jamais détenus pendant un `.await`.
pub struct LocalState {
    inner: Mutex<Inner>,
    // Réveille les requêtes longues après une action ou une reconfiguration.
    changed: Notify,
    sockets: Arc<Semaphore>,
    requests: Semaphore,
    request_rate: Mutex<RateLimit>,
    path: PathBuf,
    origins: Vec<String>,
    // Sous Windows, envoie un message au thread propriétaire de la fenêtre.
    // Dans les tests, permet de réutiliser la même API sans Win32.
    apply: Box<dyn Fn(ShortcutConfig, bool) + Send + Sync>,
}
impl LocalState {
    /// Charge et valide les préférences, ou initialise un fichier absent.
    /// Un fichier présent mais invalide provoque une erreur: il n'est pas
    /// silencieusement écrasé. Aucune session précédente n'est restaurée.
    pub fn new(
        path: PathBuf,
        origins: Vec<String>,
        apply: impl Fn(ShortcutConfig, bool) + Send + Sync + 'static,
    ) -> std::io::Result<Arc<Self>> {
        let saved = match std::fs::File::open(&path) {
            Ok(file) => {
                if !file.metadata()?.is_file() {
                    return Err(std::io::Error::other("Configuration non régulière"));
                }
                // Borner la lecture avant l'allocation, même si le fichier grossit.
                let mut bytes = Vec::new();
                file.take((SETTINGS_LIMIT + 1) as u64)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > SETTINGS_LIMIT {
                    return Err(std::io::Error::other("Configuration trop volumineuse"));
                }
                let saved: Saved = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
                if !saved.config.validate() {
                    return Err(std::io::Error::other("Configuration invalide"));
                }
                saved
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Saved {
                revision: 0,
                config: ShortcutConfig::default(),
            },
            Err(error) => return Err(error),
        };
        Ok(Arc::new(Self {
            inner: Mutex::new(Inner {
                saved,
                clients: HashMap::new(),
                events: VecDeque::new(),
                cursor: 0,
                held: false,
                applied: (u64::MAX, false),
                connections: RateLimit::new(16, 4),
                writes: RateLimit::new(8, 4),
            }),
            changed: Notify::new(),
            sockets: Arc::new(Semaphore::new(32)),
            requests: Semaphore::new(32),
            request_rate: Mutex::new(RateLimit::new(256, 128)),
            path,
            origins,
            apply: Box::new(apply),
        }))
    }
    /// Réévalue le bail et applique seulement les changements utiles au natif.
    /// Ferme PTT quand la configuration ou l’activation native change. La fermeture
    /// d’une page ne réinitialise pas les actions des autres pages actives.
    pub fn refresh(&self) {
        let change = {
            let mut inner = self.inner.lock().unwrap();
            // Le bail de quatre secondes cesse d'autoriser les actions, le
            // nettoyage de trente secondes limite séparément la table des pages.
            inner
                .clients
                .retain(|_, client| client.seen.elapsed() < Duration::from_secs(30));
            let signature = (inner.saved.revision, !inner.active_clients().is_empty());
            if inner.applied == signature {
                None
            } else {
                inner.applied = signature;
                inner.held = false;
                inner.events.clear();
                Some((inner.saved.config.clone(), signature.1))
            }
        };
        // Relâcher inner avant le callback: une implémentation native peut avoir
        // besoin d'interagir à nouveau avec l'API, sans ordre de verrous imposé.
        if let Some((config, active)) = change {
            (self.apply)(config, active);
            self.changed.notify_waiters();
        }
    }
    /// Diffuse une commande du natif aux pages actives, sauf pendant une capture.
    /// Conserve les 256 dernières actions, avec au plus seize copies par action.
    pub fn action(&self, action: Action) {
        let mut inner = self.inner.lock().unwrap();
        let clients = inner.active_clients();
        if clients.is_empty() {
            return;
        }
        let (action, pressed) = match action {
            Action::Microphone => ("microphone", true),
            Action::Headphones => ("headphones", true),
            Action::PushToTalk(pressed) => {
                inner.held = pressed;
                ("ptt", pressed)
            }
        };
        inner.cursor = inner.cursor.saturating_add(1);
        let id = inner.cursor;
        for client in clients {
            inner.events.push_back((
                client,
                Event {
                    id,
                    action,
                    pressed,
                },
            ));
        }
        let oldest = inner.cursor.saturating_sub(255);
        while inner
            .events
            .front()
            .is_some_and(|(_, event)| event.id < oldest)
        {
            inner.events.pop_front();
        }
        // Ne pas réveiller les lecteurs en conservant le verrou dont ils ont besoin.
        drop(inner);
        self.changed.notify_waiters();
    }
    // En HTTP, le jeton reste dans Authorization. En WebSocket, il est dans
    // la première trame. Il n’est accepté ni en URL ni en cookie.
    fn token(headers: &HeaderMap) -> Option<&str> {
        if headers.get_all("authorization").iter().count() != 1 {
            return None;
        }
        let token = headers
            .get("authorization")?
            .to_str()
            .ok()?
            .strip_prefix("Bearer ")?;
        (token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(token)
    }
    // Une page expirée reste identifiable un temps, mais ne redevient pas active
    // en lisant simplement des événements: elle doit repasser par POST /state.
    fn touch(&self, headers: &HeaderMap) -> Result<String, StatusCode> {
        let token = Self::token(headers).ok_or(StatusCode::UNAUTHORIZED)?;
        let mut inner = self.inner.lock().unwrap();
        let client = inner
            .clients
            .get_mut(token)
            .ok_or(StatusCode::UNAUTHORIZED)?;
        if !client.requests.allow() {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        if client.seen.elapsed() >= LEASE {
            client.active = false;
            client.recording = false;
        }
        client.seen = Instant::now();
        Ok(token.into())
    }
    // Un instantané complet permet de resynchroniser PTT/configuration même si
    // une notification est perdue. Seules les actions de la page sont incluses.
    fn snapshot(&self, token: &str, after: u64) -> Snapshot {
        let inner = self.inner.lock().unwrap();
        let active = inner.active_clients().iter().any(|client| client == token);
        let events: Vec<_> = inner
            .events
            .iter()
            .filter(|(client, event)| client == token && event.id > after)
            .map(|(_, event)| event.clone())
            .collect();
        Snapshot {
            protocol: 1,
            mouse_buttons: true,
            modifier_keys: true,
            web_socket: true,
            multi_client: true,
            requested_active: inner.clients.get(token).is_some_and(|client| client.active),
            version: env!("CARGO_PKG_VERSION"),
            revision: inner.saved.revision,
            config: inner.saved.config.clone(),
            cursor: inner.cursor,
            events,
            held: active && inner.held,
            active,
            overflow: inner
                .events
                .front()
                .is_some_and(|(_, event)| after < event.id.saturating_sub(1)),
            token: None,
        }
    }
}

/// Origines autorisées compilées depuis le domaine public central.
/// Les origines de développement ne sont ajoutées que sans profil release.
pub fn origins() -> Vec<String> {
    let mut origins = vec![env!("UHC_LINK_SITE").to_string()];
    if cfg!(debug_assertions) {
        origins.extend(
            [
                "http://127.0.0.1:5173",
                "http://127.0.0.1:4173",
                "https://localhost:4174",
            ]
            .map(str::to_string),
        );
    }
    origins
}
/// Ensemble des routes, la limite de 4 Kio s'applique aux corps HTTP.
/// La lecture initiale du fichier de préférences a sa limite distincte de 16 Kio.
pub fn router(state: Arc<LocalState>) -> Router {
    router_at_port(state, PORT)
}

fn router_at_port(state: Arc<LocalState>, port: u16) -> Router {
    Router::new()
        // Identification sans création de jeton: la recherche de ports ne
        // consomme pas les seize places disponibles pour des pages réelles.
        .route(
            "/v1/health",
            get(move || async move {
                Json(
                    serde_json::json!({"application":"uhc-link-keybinds","protocol":1,"port":port}),
                )
            }),
        )
        .route("/v1/connect", post(connect))
        .route("/v1/client", axum::routing::delete(disconnect))
        .route("/v1/state", post(set_state))
        .route("/v1/config", axum::routing::put(set_config))
        .route("/v1/events", get(events))
        .route("/v1/ws", get(websocket::upgrade))
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn_with_state((state.clone(), port), guard))
        .with_state(state)
}
async fn guard(
    State((state, port)): State<(Arc<LocalState>, u16)>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let origin = headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // Comparaison exacte du Host pour éviter d'accepter une page malveillante
    // dont un domaine aurait été résolu vers l'interface locale (DNS rebinding).
    if headers.get_all("host").iter().count() != 1
        || headers.get_all("origin").iter().count() != 1
        || host != format!("localhost:{port}")
        || !state.origins.contains(&origin)
    {
        return (StatusCode::FORBIDDEN, [("connection", "close")]).into_response();
    }
    // Le précontrôle CORS n'exige pas de jeton, les routes authentifiées
    // vérifient ensuite Authorization. Jamais d'Access-Control-Allow-Origin '*'.
    let within_rate = state.request_rate.lock().unwrap().allow();
    let mut response = if !within_rate {
        StatusCode::TOO_MANY_REQUESTS.into_response()
    } else if request.method() == Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else {
        match state.requests.try_acquire() {
            Ok(_permit) => {
                // Inclut le corps JSON et l'attente bornée du long polling.
                match tokio::time::timeout(Duration::from_secs(2), next.run(request)).await {
                    Ok(response) => response,
                    Err(_) => {
                        (StatusCode::REQUEST_TIMEOUT, [("connection", "close")]).into_response()
                    }
                }
            }
            Err(_) => StatusCode::TOO_MANY_REQUESTS.into_response(),
        }
    };
    let headers = response.headers_mut();
    headers.insert("access-control-allow-origin", origin.parse().unwrap());
    headers.insert(
        "access-control-allow-methods",
        "GET, POST, PUT, DELETE, OPTIONS".parse().unwrap(),
    );
    headers.insert(
        "access-control-allow-headers",
        "Authorization, Content-Type".parse().unwrap(),
    );
    // Autorise le précontrôle réseau privé des navigateurs qui l'utilisent,
    // leur éventuelle permission utilisateur reste gérée par le navigateur.
    headers.insert(
        "access-control-allow-private-network",
        "true".parse().unwrap(),
    );
    headers.insert("access-control-max-age", "600".parse().unwrap());
    headers.insert("cache-control", "no-store".parse().unwrap());
    headers.insert("vary", "Origin".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    response
}
// Ouvre une page inactive: lire la configuration ne réserve pas les raccourcis.
// Le jeton aléatoire de 256 bits est éphémère et n'est jamais écrit sur disque.
async fn connect(State(state): State<Arc<LocalState>>) -> Result<Json<Snapshot>, StatusCode> {
    state.refresh();
    let mut bytes = [0; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let token: String = bytes.iter().map(|v| format!("{v:02x}")).collect();
    let mut inner = state.inner.lock().unwrap();
    if inner.clients.len() >= 16 || !inner.connections.allow() {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    inner.clients.insert(
        token.clone(),
        Client {
            active: false,
            recording: false,
            websocket: false,
            seen: Instant::now(),
            requests: RateLimit::new(60, 30),
        },
    );
    drop(inner);
    let mut snapshot = state.snapshot(&token, 0);
    snapshot.token = Some(token);
    snapshot.events.clear();
    Ok(Json(snapshot))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Active {
    active: bool,
    #[serde(default)]
    recording: bool,
}
// La suppression explicite ne supprime aucune préférence du PC.
async fn disconnect(
    State(state): State<Arc<LocalState>>,
    headers: HeaderMap,
) -> Result<Json<Disconnected>, StatusCode> {
    let token = state.touch(&headers)?;
    state.inner.lock().unwrap().clients.remove(&token);
    state.refresh();
    state.changed.notify_waiters();
    Ok(Json(Disconnected { ok: true }))
}
// L'activation est indépendante pour chaque page, la configuration reste commune.
async fn set_state(
    State(state): State<Arc<LocalState>>,
    headers: HeaderMap,
    Json(value): Json<Active>,
) -> Result<Json<Snapshot>, StatusCode> {
    let token = state.touch(&headers)?;
    {
        let mut inner = state.inner.lock().unwrap();
        let client = inner
            .clients
            .get_mut(&token)
            .ok_or(StatusCode::UNAUTHORIZED)?;
        client.active = value.active;
        client.recording = value.recording;
        if !value.active {
            inner.events.retain(|(client, _)| client != &token);
        }
    }
    state.refresh();
    state.changed.notify_waiters();
    Ok(Json(state.snapshot(&token, u64::MAX)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    revision: u64,
    config: ShortcutConfig,
}
// Contrôle optimiste de révision: une page ne peut pas écraser des réglages
// qu'une autre vient de modifier sans avoir relu la nouvelle révision.
async fn set_config(
    State(state): State<Arc<LocalState>>,
    headers: HeaderMap,
    Json(update): Json<Update>,
) -> Result<Json<Snapshot>, StatusCode> {
    let token = state.touch(&headers)?;
    if !update.config.validate() {
        return Err(StatusCode::BAD_REQUEST);
    }
    {
        let mut inner = state.inner.lock().unwrap();
        if inner.saved.revision != update.revision {
            return Err(StatusCode::CONFLICT);
        }
        if !inner.clients.contains_key(&token) {
            return Err(StatusCode::UNAUTHORIZED);
        }
        // Un budget commun reste borné même si une page recrée ses jetons.
        if !inner.writes.allow() {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        // Une révision issue d'un fichier local ne doit jamais faire paniquer Rust.
        let revision = inner
            .saved
            .revision
            .checked_add(1)
            .ok_or(StatusCode::CONFLICT)?;
        let saved = Saved {
            revision,
            config: update.config,
        };
        if let Some(parent) = state.path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        // Écrire le fichier voisin puis le renommer évite de tronquer l'ancien
        // JSON en cours d'écriture. Ce n'est pas une garantie fsync contre une
        // coupure électrique. L'état mémoire n'est publié qu'après succès disque.
        persist(&state.path, &saved).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        inner.saved = saved;
    }
    state.refresh();
    Ok(Json(state.snapshot(&token, u64::MAX)))
}

// Un fichier temporaire imprévisible créé exclusivement évite de suivre un lien
// préexistant nommé settings.next. Il reste voisin pour le renommage atomique.
fn persist(path: &std::path::Path, saved: &Saved) -> std::io::Result<()> {
    use std::io::Write;
    let mut random = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut random);
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let next = path.with_file_name(format!("settings.{suffix}.next"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Ne nettoyer que le fichier dont cette opération a obtenu la création.
    let mut file = options.open(&next)?;
    let result = (|| {
        file.write_all(&serde_json::to_vec_pretty(saved)?)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&next, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&next);
    }
    result
}
#[derive(Deserialize)]
struct Cursor {
    after: u64,
}
// Long polling: attend une notification ou rend un instantané périodique.
// L'attente plus courte pendant PTT renouvelle rapidement l'état de transmission.
async fn events(
    State(state): State<Arc<LocalState>>,
    headers: HeaderMap,
    Query(cursor): Query<Cursor>,
) -> Result<Json<Snapshot>, StatusCode> {
    let token = state.touch(&headers)?;
    let notified = state.changed.notified();
    tokio::pin!(notified);
    // Inscrire le réveil avant de tester le curseur ferme la fenêtre de course
    // où une action arriverait entre la lecture de l'état et la mise en attente.
    notified.as_mut().enable();
    let (ready, held) = {
        let inner = state.inner.lock().unwrap();
        (inner.cursor > cursor.after, inner.held)
    };
    if !ready {
        let _ = tokio::time::timeout(
            Duration::from_millis(if held { 200 } else { 800 }),
            notified,
        )
        .await;
    }
    // Vérifier à nouveau après l'attente: une page peut avoir été supprimée.
    state.touch(&headers)?;
    state.refresh();
    Ok(Json(state.snapshot(&token, cursor.after)))
}
/// Démarre l'API sans canal de notification de disponibilité, utile aux tests.
pub async fn serve(state: Arc<LocalState>) -> std::io::Result<()> {
    serve_with_ready(state, None).await
}
/// Écoute le premier port disponible sur localhost et signale le port choisi.
/// Les listeners sont conservés entre sélection et démarrage: pas de test de
/// disponibilité suivi d'une fermeture qui laisserait une course avec un autre app.
pub async fn serve_with_ready(
    state: Arc<LocalState>,
    ready: Option<std::sync::mpsc::SyncSender<u16>>,
) -> std::io::Result<()> {
    let (ipv4, ipv6) = bind_loopback(PORT..=LAST_PORT).await?;
    let port = ipv4.local_addr()?.port();
    let app = router_at_port(state.clone(), port);
    let connections = Arc::new(Semaphore::new(64));
    if let Some(ipv6) = ipv6 {
        let app = app.clone();
        let connections = connections.clone();
        tokio::spawn(async move {
            let _ = transport::serve(ipv6, app, connections).await;
        });
    }
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_millis(500));
        loop {
            timer.tick().await;
            state.refresh();
        }
    });
    // Le thread Windows attend ce signal avant d'afficher l'icône comme active.
    if let Some(ready) = ready {
        let _ = ready.send(port);
    }
    transport::serve(ipv4, app, connections).await
}

/// Réserve IPv4 et, quand disponible, IPv6 sur le même port de boucle locale.
/// Un port occupé ou réservé par Windows est sauté. Une collision IPv6 fait
/// aussi passer au port suivant, afin que localhost ne désigne pas deux services.
async fn bind_loopback(
    ports: std::ops::RangeInclusive<u16>,
) -> std::io::Result<(tokio::net::TcpListener, Option<tokio::net::TcpListener>)> {
    use std::io::ErrorKind;
    use tokio::net::{TcpListener, TcpSocket};
    let first = *ports.start();
    let last = *ports.end();
    let ipv6_supported = TcpSocket::new_v6().is_ok();
    for port in ports {
        let ipv4 = match TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await {
            Ok(listener) => listener,
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::AddrInUse | ErrorKind::PermissionDenied
                ) =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        let ipv6 = if ipv6_supported {
            match TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port)).await {
                Ok(listener) => Some(listener),
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::AddrInUse | ErrorKind::PermissionDenied
                    ) =>
                {
                    continue
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::AddrNotAvailable | ErrorKind::Unsupported
                    ) =>
                {
                    None
                }
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        return Ok((ipv4, ipv6));
    }
    Err(std::io::Error::new(
        ErrorKind::AddrInUse,
        format!("Aucun port local disponible entre {first} et {last}."),
    ))
}

#[cfg(test)]
mod port_tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    // Réserver un bloc éphémère évite toute dépendance à un port du poste de test.
    fn reserve_three() -> Vec<std::net::TcpListener> {
        for _ in 0..100 {
            let first = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = first.local_addr().unwrap().port();
            if port > u16::MAX - 2 {
                continue;
            }
            if let (Ok(second), Ok(third)) = (
                std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port + 1)),
                std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port + 2)),
            ) {
                return vec![first, second, third];
            }
        }
        panic!("Impossible de réserver un bloc de test");
    }

    #[tokio::test]
    async fn skips_occupied_ports_and_accepts_the_last_inclusive_port() {
        let mut held = reserve_three();
        let first = held[0].local_addr().unwrap().port();
        let last = first + 2;
        drop(held.pop());
        let (ipv4, ipv6) = bind_loopback(first..=last).await.unwrap();
        assert_eq!(ipv4.local_addr().unwrap().port(), last);
        assert!(ipv4.local_addr().unwrap().ip().is_loopback());
        if let Some(ipv6) = ipv6 {
            assert_eq!(ipv6.local_addr().unwrap().port(), last);
            assert!(ipv6.local_addr().unwrap().ip().is_loopback());
        }
    }

    #[tokio::test]
    async fn exhausted_range_returns_an_error_without_using_an_extra_port() {
        let held = reserve_three();
        let first = held[0].local_addr().unwrap().port();
        let result = bind_loopback(first..=first + 2).await.unwrap_err();
        assert_eq!(result.kind(), std::io::ErrorKind::AddrInUse);
        assert!(result.to_string().contains(&(first + 2).to_string()));
    }

    #[tokio::test]
    async fn ipv6_collision_does_not_leave_an_ipv4_only_service_on_that_port() {
        let Ok(held) = std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)) else {
            return;
        };
        let port = held.local_addr().unwrap().port();
        let result = bind_loopback(port..=port).await.unwrap_err();
        assert_eq!(result.kind(), std::io::ErrorKind::AddrInUse);
        assert!(std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok());
    }

    #[tokio::test]
    async fn discovery_is_stateless_and_host_must_match_the_selected_port() {
        let dir = tempfile::tempdir().unwrap();
        let origin = "https://uhc-link.fr";
        let state = LocalState::new(
            dir.path().join("settings.json"),
            vec![origin.into()],
            |_, _| {},
        )
        .unwrap();
        let app = router_at_port(state.clone(), LAST_PORT);
        for host in [
            format!("localhost:{LAST_PORT}"),
            format!("localhost:{PORT}"),
            "evil.example:32245".into(),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::get("/v1/health")
                        .header("origin", origin)
                        .header("host", &host)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            if host == format!("localhost:{LAST_PORT}") {
                assert_eq!(response.status(), StatusCode::OK);
                let data = response.into_body().collect().await.unwrap().to_bytes();
                let body: serde_json::Value = serde_json::from_slice(&data).unwrap();
                assert_eq!(body["application"], "uhc-link-keybinds");
                assert_eq!(body["port"], LAST_PORT);
            } else {
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
            }
        }
        assert!(state.inner.lock().unwrap().clients.is_empty());
    }
}
