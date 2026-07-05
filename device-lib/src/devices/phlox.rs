//! Драйвер устройства Phlox (VID=0x16C0 PID=0x05DC).
//!
//! В отличие от семейства АППИ (см. `appi_common.rs`), Phlox общается
//! потоком кадров `ID|len|CRC8|data` (см. `phlox_common.rs`), а не сырыми
//! пакетами фиксированного размера. Набор модулей (CAN/UART/Gen) устройство
//! сообщает само в ответе "Device Info" — здесь их количество не фиксировано
//! заранее, как у АППИ, поэтому `CanCapability`/`UartCapability`/
//! `GenCapability` для Phlox пока не реализованы:
//! этот файл делает только то, что нужно для первого подключения —
//! рукопожатие Master (Start → Device Info) и чтение версии.
//!
//! Bulk-эндпоинты и номер интерфейса устройства заранее не известны
//! (в отличие от АППИ), поэтому определяются динамически из дескриптора
//! интерфейса при открытии — см. [`discover_bulk_endpoints`].

use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nusb::descriptors::TransferType;
use nusb::transfer::{Bulk, Buffer, Direction, In, Out};
use nusb::Interface;

use crate::devices::phlox_common as pc;
use crate::error::DeviceError;
use crate::traits::{CapabilitySet, Device};

const DEFAULT_INTERFACE: u8 = 0;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const READ_CHUNK: usize = 512;

#[derive(Default)]
struct PhloxState {
    // накопительный буфер: кадр может быть разрезан USB-транзакциями
    rx_buf: Vec<u8>,
    next_rid: u8,
}

pub struct Phlox {
    iface: Interface,
    write_ep: u8,
    read_ep: u8,
    state: Mutex<PhloxState>,
    // гарантирует, что в каждый момент времени идёт не более одной USB-транзакции
    usb_lock: tokio::sync::Mutex<()>,
}

unsafe impl Send for Phlox {}
unsafe impl Sync for Phlox {}

fn log_configuration(device: &nusb::Device) {
    let cfg = match device.active_configuration() {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!("Phlox: не удалось прочитать active_configuration: {e}");
            return;
        }
    };
    for iface in cfg.interfaces() {
        for alt in iface.alt_settings() {
            let eps: Vec<String> = alt
                .endpoints()
                .map(|e| {
                    format!(
                        "{:?} addr=0x{:02X} type={:?} max_packet={}",
                        e.direction(), e.address(), e.transfer_type(), e.max_packet_size()
                    )
                })
                .collect();
            tracing::info!(
                "Phlox: конфигурация — интерфейс {} alt={} class=0x{:02X} sub=0x{:02X} proto=0x{:02X} эндпоинты={:?}",
                alt.interface_number(), alt.alternate_setting(),
                alt.class(), alt.subclass(), alt.protocol(), eps
            );
        }
    }
}

impl Phlox {
    pub async fn open(device: nusb::Device) -> Result<Self, DeviceError> {
        log_configuration(&device);

        tracing::info!("Phlox: захватываю интерфейс {DEFAULT_INTERFACE}");

        #[cfg(target_os = "linux")]
        let iface = device
            .detach_and_claim_interface(DEFAULT_INTERFACE)
            .await
            .map_err(DeviceError::from)?;

        #[cfg(not(target_os = "linux"))]
        let iface = device
            .claim_interface(DEFAULT_INTERFACE)
            .await
            .map_err(DeviceError::from)?;

        let (write_ep, read_ep) = discover_bulk_endpoints(&iface)?;
        tracing::info!("Phlox: интерфейс {DEFAULT_INTERFACE}, OUT=0x{write_ep:02X} IN=0x{read_ep:02X}");

        Ok(Self {
            iface,
            write_ep,
            read_ep,
            state: Mutex::new(PhloxState::default()),
            usb_lock: tokio::sync::Mutex::new(()),
        })
    }

    async fn usb_write(&self, data: &[u8]) -> Result<(), DeviceError> {
        let mut ep = self
            .iface
            .endpoint::<Bulk, Out>(self.write_ep)
            .map_err(|e| DeviceError::Usb(e.to_string()))?;
        ep.submit(data.to_vec().into());
        ep.next_complete().await.into_result().map_err(DeviceError::from)?;
        Ok(())
    }

    async fn usb_read_chunk(&self) -> Result<Vec<u8>, DeviceError> {
        let mut ep = self
            .iface
            .endpoint::<Bulk, In>(self.read_ep)
            .map_err(|e| DeviceError::Usb(e.to_string()))?;
        ep.submit(Buffer::new(READ_CHUNK));
        let data = ep.next_complete().await.into_result().map_err(DeviceError::from)?;
        Ok(data.to_vec())
    }

    async fn read_frame(&self, deadline: Instant) -> Result<pc::Frame, DeviceError> {
        loop {
            {
                let mut st = self.state.lock().unwrap();
                if let Some((frame, consumed)) = pc::find_frame(&st.rx_buf) {
                    st.rx_buf.drain(..consumed);
                    return Ok(frame);
                }
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(DeviceError::Timeout);
            }

            let chunk = match tokio::time::timeout(remaining, self.usb_read_chunk()).await {
                Ok(res) => res?,
                Err(_) => return Err(DeviceError::Timeout),
            };
            if chunk.is_empty() {
                continue;
            }
            self.state.lock().unwrap().rx_buf.extend_from_slice(&chunk);
        }
    }

    async fn handshake(&self) -> Result<pc::DeviceInfoMsg, DeviceError> {
        let rid = {
            let mut st = self.state.lock().unwrap();
            st.next_rid = st.next_rid.wrapping_add(1);
            st.next_rid
        };

        self.usb_write(&pc::start_frame(rid)).await?;

        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            let frame = self.read_frame(deadline).await?;
            if frame.module_id != pc::MODULE_MASTER {
                continue;
            }
            match pc::parse_device_info(&frame.data) {
                Ok(info) if info.rid == rid => return Ok(info),
                _ => continue,
            }
        }
    }
}

fn hex_dump(data: &[u8]) -> String {
    const LIMIT: usize = 64;
    let shown = &data[..data.len().min(LIMIT)];
    let mut s: String = shown.iter().map(|b| format!("{b:02X} ")).collect();
    if data.len() > LIMIT {
        s.push_str(&format!("... (+{} байт)", data.len() - LIMIT));
    }
    s
}

#[async_trait]
impl Device for Phlox {
    async fn get_version(&self) -> Result<String, DeviceError> {
        let _guard = self.usb_lock.lock().await;
        let info = self.handshake().await?;
        tracing::info!(
            "Phlox: версия {}.{} (совместимость с {}), модулей: {}",
            info.version, info.subversion, info.compat_version, info.modules.len()
        );
        for m in &info.modules {
            tracing::info!("Phlox: модуль id={} type={} name={:?}", m.id, m.kind, m.name);
        }
        Ok(format!("{}.{}", info.version, info.subversion))
    }

    async fn reset(&self) -> Result<(), DeviceError> {
        let _guard = self.usb_lock.lock().await;
        self.handshake().await?;
        Ok(())
    }

    fn capabilities(&self) -> CapabilitySet {
        // MVP: только версия/хендшейк. CAN/UART/Gen для Phlox добавляются
        // отдельно, когда согласован разбор их сообщений.
        CapabilitySet::NONE
    }
}

fn discover_bulk_endpoints(iface: &Interface) -> Result<(u8, u8), DeviceError> {
    let desc = iface
        .descriptor()
        .ok_or_else(|| DeviceError::Protocol("нет дескриптора интерфейса".into()))?;

    let mut out_ep = None;
    let mut in_ep = None;
    for ep in desc.endpoints() {
        if ep.transfer_type() != TransferType::Bulk {
            continue;
        }
        match ep.direction() {
            Direction::Out if out_ep.is_none() => out_ep = Some(ep.address()),
            Direction::In  if in_ep.is_none()  => in_ep  = Some(ep.address()),
            _ => {}
        }
    }

    match (out_ep, in_ep) {
        (Some(o), Some(i)) => Ok((o, i)),
        _ => Err(DeviceError::Protocol(
            "не найдена пара bulk-эндпоинтов IN/OUT на интерфейсе".into(),
        )),
    }
}
