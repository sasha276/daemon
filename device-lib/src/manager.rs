use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use nusb::MaybeFuture;

use crate::devices::appi::Appi;
use crate::devices::appi2::{Appi2, Appi2Variant};
use crate::devices::phlox::Phlox;
use crate::{error::DeviceError, traits::Device, DeviceId, DeviceInfo};

const APPI_VID: u16 = 0x1992;
const APPI_PID: u16 = 0x1972;
const BCD_APPI2M: u16 = 0x7300;

const PHLOX_VID:u16=0x16C0;
const PHLOX_PID:u16=0x05DC;

const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum DeviceKind {
    Appi,
    Appi2,
    Appi2M,
    Phlox,
}


/// Одна запись в списке устройств.
///
/// ВАЖНО: перечисление (`refresh`) НЕ открывает устройство. Мы лишь
/// запоминаем его `nusb::DeviceInfo` и тип. Реальный `open()` /
/// `claim_interface()` происходит лениво в `get_driver()` и кешируется в
/// `driver`. Это убирает повторный claim на каждый refresh и устраняет
/// гонку при двух одинаковых VID:PID, когда один прибор не успевал
/// освободиться и отсеивался.
pub struct DeviceHandle {
    pub info: DeviceInfo,
    raw: nusb::DeviceInfo,
    kind: DeviceKind,
    driver: Option<Arc<dyn Device>>,
}

pub struct DeviceManager {
    devices: RwLock<Vec<DeviceHandle>>,
}

impl DeviceManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            devices: RwLock::new(vec![]),
        })
    }

    /// Перечисляет ВСЕ подключённые приборы АППИ и обновляет список.
    /// Ничего не открывает — только составляет перечень. Уже открытые
    /// драйверы, чьи приборы остались на месте (тот же bus+port), сохраняются,
    /// чтобы не рвать активные сессии при каждом refresh.
    pub async fn refresh(&self) {
        let mut list = self.devices.write().await;
        let mut old: Vec<DeviceHandle> = std::mem::take(&mut *list);

        let device_list = match nusb::list_devices().wait() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("nusb::list_devices() error: {e}");
                return;
            }
        };

        let all: Vec<nusb::DeviceInfo> = device_list.collect();

        tracing::info!("enumerate: {} usb device(s) total", all.len());

        for info in all {
            let info: nusb::DeviceInfo = info;
            let vid = info.vendor_id();
            let pid = info.product_id();

            let kind = if vid == APPI_VID && pid == APPI_PID {
                classify_appi(info.device_version())
            } else if vid == PHLOX_VID && pid == PHLOX_PID {
                DeviceKind::Phlox
            } else {
                continue;
            };

            let bcd_raw = info.device_version();

            let id_bus = info.device_address();
            let id_port = info
                .port_chain()
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(".");
            let id_vid = vid;
            let id_pid = pid;
            let id_bus_id = info.bus_id().to_string();
            let manufacturer = info.manufacturer_string().unwrap_or("Appi").to_string();

            let product_name = match kind {
                DeviceKind::Appi2M => Appi2Variant::Appi2M.name().to_string(),
                DeviceKind::Appi2 => Appi2Variant::Appi2.name().to_string(),
                DeviceKind::Appi => "АППИ".to_string(),
                DeviceKind::Phlox => "Phlox".to_string(),
            };

            let dev_info = DeviceInfo {
                id: DeviceId {
                    vid: id_vid,
                    pid: id_pid,
                    bus: id_bus,
                    port: id_port.clone(),
                },
                manufacturer,
                product: product_name,
            };

            let reused = old
                .iter()
                .position(|h| h.info.id.bus == id_bus && h.info.id.port == id_port)
                .and_then(|pos| old.remove(pos).driver);

            tracing::info!(
                "found {} at bus_id={} addr={} port={} (bcd={:04X})",
                dev_info.product,
                id_bus_id,
                id_bus,
                dev_info.id.port,
                bcd_raw
            );

            list.push(DeviceHandle {
                info: dev_info,
                raw: info,
                kind,
                driver: reused,
            });
        }

        tracing::info!("refresh complete: {} device(s)", list.len());
    }

    pub async fn list_info(&self) -> Vec<DeviceInfo> {
        self.devices
            .read()
            .await
            .iter()
            .map(|h| h.info.clone())
            .collect()
    }

    pub async fn get_driver(&self, idx: usize) -> Result<Arc<dyn Device>, DeviceError> {
        {
            let list = self.devices.read().await;
            let h = list.get(idx).ok_or(DeviceError::NotFound)?;
            if let Some(d) = &h.driver {
                return Ok(d.clone());
            }
        }

        // Медленный путь: открываем под write-локом.
        let mut list = self.devices.write().await;
        let h = list.get_mut(idx).ok_or(DeviceError::NotFound)?;
        if let Some(d) = &h.driver {
            return Ok(d.clone());
        }

        let raw = h.raw.clone();
        let device = tokio::time::timeout(OPEN_TIMEOUT, async { raw.open().await })
            .await
            .map_err(|_| DeviceError::Timeout)?
            .map_err(DeviceError::from)?;
        let driver: Arc<dyn Device> = match h.kind {
            DeviceKind::Appi2M => Arc::new(Appi2::open(device, Appi2Variant::Appi2M)?),
            DeviceKind::Appi2 => Arc::new(Appi2::open(device, Appi2Variant::Appi2)?),
            DeviceKind::Appi => Arc::new(Appi::open(device)?),
            DeviceKind::Phlox => {
                let phlox = tokio::time::timeout(OPEN_TIMEOUT, Phlox::open(device))
                    .await
                    .map_err(|_| DeviceError::Timeout)??;
                Arc::new(phlox)
            }
        };
        h.driver = Some(driver.clone());
        Ok(driver)
    }

    pub async fn get_info(&self, idx: usize) -> Option<DeviceInfo> {
        self.devices.read().await.get(idx).map(|h| h.info.clone())
    }
}

fn classify_appi(bcd_raw: u16) -> DeviceKind {
    if bcd_raw == BCD_APPI2M {
        return DeviceKind::Appi2M;
    }
    DeviceKind::Appi2
}