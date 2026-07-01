use std::net::SocketAddr;
use dashmap::DashMap;
use std::sync::atomic::{AtomicU16, Ordering};

#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub id:   u16,
    pub addr: SocketAddr,
}

/// Thread-safe registry: addr ↔ client_id
pub struct ClientRegistry {
    by_addr: DashMap<SocketAddr, u16>,
    by_id:   DashMap<u16, ClientInfo>,
    next_id: AtomicU16,
}

impl ClientRegistry {
    pub fn new() -> Self {
        Self {
            by_addr: DashMap::new(),
            by_id:   DashMap::new(),
            next_id: AtomicU16::new(1),
        }
    }

    /// Register a new client; returns assigned id.
    /// If addr already registered returns existing id.
    pub fn register(&self, addr: SocketAddr) -> u16 {
        if let Some(id) = self.by_addr.get(&addr) {
            return *id;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.by_addr.insert(addr, id);
        self.by_id.insert(id, ClientInfo { id, addr });
        tracing::info!("client registered id={id} addr={addr}");
        id
    }

    pub fn unregister(&self, id: u16) {
        if let Some((_, info)) = self.by_id.remove(&id) {
            self.by_addr.remove(&info.addr);
            tracing::info!("client unregistered id={id}");
        }
    }

    pub fn get_by_id(&self, id: u16) -> Option<ClientInfo> {
        self.by_id.get(&id).map(|e| e.clone())
    }

    pub fn get_by_addr(&self, addr: &SocketAddr) -> Option<u16> {
        self.by_addr.get(addr).map(|e| *e)
    }

    pub fn update_addr(&self, id: u16, new_addr: SocketAddr) {
        if let Some(mut info) = self.by_id.get_mut(&id) {
            let old = info.addr;
            self.by_addr.remove(&old);
            self.by_addr.insert(new_addr, id);
            info.addr = new_addr;
        }
    }
}
