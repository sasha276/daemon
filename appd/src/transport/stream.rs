use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use crate::error::AppError;

// ── стрим-порт (CAN read) ─────────────────────────────────────────────────────

pub struct StreamPort {
    pub local_port:       u16,
    pub client_data_addr: SocketAddr,
    pub socket:           Arc<UdpSocket>,
    _handle:              JoinHandle<()>,
}

// ── менеджер ──────────────────────────────────────────────────────────────────

pub struct StreamManager {
    /// Активные стрим-порты: (client_id, session_id) → порт
    ports:    Mutex<HashMap<(u16, u16), StreamPort>>,
    /// Periodic CAN задачи: (client_id, session_id) → stop sender
    periodic: Mutex<HashMap<(u16, u16), oneshot::Sender<()>>>,
}

impl StreamManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            ports:    Mutex::new(HashMap::new()),
            periodic: Mutex::new(HashMap::new()),
        })
    }

    // ── stream ports ──────────────────────────────────────────────────────

    /// Открывает эфемерный UDP порт. Возвращает номер порта.
    pub async fn open(
        &self,
        client_id:        u16,
        session_id:       u16,
        client_data_addr: SocketAddr,
    ) -> Result<u16, AppError> {
        let socket     = UdpSocket::bind("0.0.0.0:0").await?;
        let local_port = socket.local_addr()?.port();
        let socket     = Arc::new(socket);

        // rx loop — данные от клиента на этот порт (входящие CAN write)
        let sock_rx = socket.clone();
        let handle  = tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            loop {
                match sock_rx.recv_from(&mut buf).await {
                    Ok((len, addr)) => {
                        tracing::debug!("stream port {local_port} rx {len}B from {addr}");
                        // TODO: передать входящие данные в can_write устройства
                    }
                    Err(e) => {
                        tracing::warn!("stream port {local_port} error: {e}");
                        break;
                    }
                }
            }
        });

        self.ports.lock().await.insert(
            (client_id, session_id),
            StreamPort { local_port, client_data_addr, socket, _handle: handle },
        );

        tracing::info!(
            "stream opened client={client_id} session={session_id} \
             local={local_port} -> {client_data_addr}"
        );
        Ok(local_port)
    }

    /// Пушит сырые байты фреймов клиенту на его data-порт.
    pub async fn push_frame(&self, client_id: u16, session_id: u16, frame: &[u8]) {
        let ports = self.ports.lock().await;
        if let Some(port) = ports.get(&(client_id, session_id)) {
            if let Err(e) = port.socket.send_to(frame, port.client_data_addr).await {
                tracing::warn!("push_frame error: {e}");
            }
        }
    }

    pub async fn close(&self, client_id: u16, session_id: u16) {
        self.stop_periodic(client_id, session_id).await;
        self.ports.lock().await.remove(&(client_id, session_id));
        tracing::info!("stream closed client={client_id} session={session_id}");
    }

    pub async fn close_all_for_client(&self, client_id: u16) {
        // остановить все periodic задачи клиента
        let keys: Vec<_> = self.periodic.lock().await
            .keys()
            .filter(|(cid, _)| *cid == client_id)
            .cloned()
            .collect();
        for (cid, sid) in keys {
            self.stop_periodic(cid, sid).await;
        }
        // закрыть все порты
        self.ports.lock().await.retain(|(cid, _), _| *cid != client_id);
    }

    // ── periodic tasks ────────────────────────────────────────────────────

    /// Сохраняет stop-канал для periodic задачи.
    pub async fn add_periodic(
        &self,
        client_id:  u16,
        session_id: u16,
        stop_tx:    oneshot::Sender<()>,
    ) {
        // если уже была — остановить старую
        self.stop_periodic(client_id, session_id).await;
        self.periodic.lock().await.insert((client_id, session_id), stop_tx);
    }

    /// Останавливает periodic задачу если есть.
    pub async fn stop_periodic(&self, client_id: u16, session_id: u16) {
        if let Some(tx) = self.periodic.lock().await.remove(&(client_id, session_id)) {
            let _ = tx.send(());
        }
    }
}
