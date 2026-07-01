use std::sync::atomic::{AtomicU16, Ordering};
use dashmap::DashMap;
use crate::error::AppError;

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id:        u16,
    pub client_id: u16,
    /// USB device index in device list at time of creation
    pub device_idx: usize,
    /// bus:port string for disambiguation
    pub device_key: String,
}

pub struct SessionManager {
    sessions: DashMap<u16, SessionInfo>,
    next_id:  AtomicU16,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            next_id:  AtomicU16::new(1),
        }
    }

    pub fn create(&self, client_id: u16, device_idx: usize, device_key: String) -> Result<u16, AppError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.sessions.insert(id, SessionInfo { id, client_id, device_idx, device_key });
        tracing::info!("session created id={id} client={client_id} device={device_idx}");
        Ok(id)
    }

    pub fn close(&self, id: u16) -> Result<(), AppError> {
        self.sessions.remove(&id).ok_or(AppError::UnknownSession(id))?;
        tracing::info!("session closed id={id}");
        Ok(())
    }

    pub fn get(&self, id: u16) -> Result<SessionInfo, AppError> {
        self.sessions.get(&id).map(|e| e.clone()).ok_or(AppError::UnknownSession(id))
    }

    pub fn list_for_client(&self, client_id: u16) -> Vec<SessionInfo> {
        self.sessions
            .iter()
            .filter(|e| e.client_id == client_id)
            .map(|e| e.clone())
            .collect()
    }

    pub fn close_all_for_client(&self, client_id: u16) {
        let ids: Vec<u16> = self.sessions
            .iter()
            .filter(|e| e.client_id == client_id)
            .map(|e| *e.key())
            .collect();
        for id in ids {
            self.sessions.remove(&id);
        }
    }
}
