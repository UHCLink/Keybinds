//! Limites du transport avant même l'authentification HTTP ou WebSocket.
//! Le permis accompagne aussi une connexion mise à niveau, via ses extensions.
//! IPv4 et IPv6 partagent donc le même plafond de sockets réellement ouvertes.
use super::*;
use axum::Extension;
use hyper::server::conn::http1;
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use tokio::net::TcpListener;
use tokio::sync::OwnedSemaphorePermit;

pub(super) type ConnectionPermit = Arc<OwnedSemaphorePermit>;

pub(super) async fn serve(
    listener: TcpListener,
    app: Router,
    connections: Arc<Semaphore>,
) -> std::io::Result<()> {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(connection) => connection,
            Err(_) => {
                // Éviter une boucle active si le système manque de ressources.
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            // Refuser immédiatement, sans créer une file de tâches en attente.
            continue;
        };
        let _ = stream.set_nodelay(true);
        let permit = Arc::new(permit);
        let service = TowerToHyperService::new(app.clone().layer(Extension(permit.clone())));
        tokio::spawn(async move {
            let _permit = permit;
            let mut builder = http1::Builder::new();
            builder
                .timer(TokioTimer::new())
                .header_read_timeout(Duration::from_secs(5))
                .max_headers(32)
                .max_buf_size(8192);
            let _ = builder
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades()
                .await;
        });
    }
}
