/*use std::sync::Arc;
use tokio::sync::RwLock;

use nusb::MaybeFuture;

use crate::devices::appi::Appi;
use crate::devices::appi2::{Appi2, Appi2Variant};
use crate::{error::DeviceError, traits::Device, DeviceId, DeviceInfo};

const APPI_VID: u16 = 0x1992;
const APPI_PID: u16 = 0x1972;
const BCD_APPI2M: u16 = 0x7300;

/// К какому драйверу относится найденное устройство.
#[derive(Clone, Copy)]
enum AppiKind {
    Appi,
    Appi2,
    Appi2M,
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
    /// Сохранённое описание конкретного физического прибора для ленивого open.
    raw: nusb::DeviceInfo,
    kind: AppiKind,
    /// Кеш открытого драйвера (открывается при первом обращении).
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

        // Забираем прежние открытые драйверы, чтобы переиспользовать их для
        // приборов, оставшихся на тех же физических портах.
        let mut old: Vec<DeviceHandle> = std::mem::take(&mut *list);

        let device_list = match nusb::list_devices().wait() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("nusb::list_devices() error: {e}");
                return;
            }
        };

        // Диагностика: материализуем список и считаем, сколько приборов с нашим
        // VID:PID реально видит ОС. Если здесь 1 при двух подключённых — проблема
        // не в коде, а в драйвере/хабе/композитном устройстве на стороне ОС.
        let all: Vec<nusb::DeviceInfo> = device_list.collect();
        let matching = all
            .iter()
            .filter(|d| d.vendor_id() == APPI_VID && d.product_id() == APPI_PID)
            .count();
        tracing::info!(
            "enumerate: {} usb device(s) total, {} with VID:PID {:04X}:{:04X}",
            all.len(),
            matching,
            APPI_VID,
            APPI_PID
        );

        for info in all {
            let info: nusb::DeviceInfo = info;
            if info.vendor_id() != APPI_VID || info.product_id() != APPI_PID {
                continue;
            }

            let bcd_raw = info.device_version();
            let product_str = info.product_string().map(|s: &str| s.to_string());

            // bus + port уникальны для каждого физического прибора, даже когда
            // VID/PID/serial совпадают. По ним и различаем устройства.
            let id_bus = info.device_address();
            let id_port = info.port_number().to_string();
            let id_vid = info.vendor_id();
            let id_pid = info.product_id();
            let id_bus_id = info.bus_id().to_string();
            let manufacturer = info.manufacturer_string().unwrap_or("Appi").to_string();

            let kind = classify(bcd_raw, product_str.as_deref());
            let product_name = match kind {
                AppiKind::Appi2M => Appi2Variant::Appi2M.name().to_string(),
                AppiKind::Appi2 => Appi2Variant::Appi2.name().to_string(),
                AppiKind::Appi => "АППИ".to_string(),
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

            // Если этот же прибор уже был открыт в прошлый раз (тот же bus+port),
            // переносим живой драйвер — не дёргаем USB и не рвём сессию.
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

    /// Возвращает драйвер устройства по индексу, открывая его при первом
    /// обращении (ленивое открытие) и кешируя результат.
    pub async fn get_driver(&self, idx: usize) -> Result<Arc<dyn Device>, DeviceError> {
        // Быстрый путь: драйвер уже открыт.
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
        let device = raw.open().wait().map_err(DeviceError::from)?;
        let driver: Arc<dyn Device> = match h.kind {
            AppiKind::Appi2M => Arc::new(Appi2::open(device, Appi2Variant::Appi2M)?),
            AppiKind::Appi2 => Arc::new(Appi2::open(device, Appi2Variant::Appi2)?),
            AppiKind::Appi => Arc::new(Appi::open(device)?),
        };
        h.driver = Some(driver.clone());
        Ok(driver)
    }

    pub async fn get_info(&self, idx: usize) -> Option<DeviceInfo> {
        self.devices.read().await.get(idx).map(|h| h.info.clone())
    }
}

fn classify(bcd_raw: u16, product: Option<&str>) -> AppiKind {
    if bcd_raw == BCD_APPI2M {
        return AppiKind::Appi2M;
    }
    if let Some(p) = product {
        if p.contains('2') {
            return AppiKind::Appi2;
        }
    }
    AppiKind::Appi
}
*/

use std::sync::Arc;
use tokio::sync::RwLock;

use nusb::MaybeFuture;

use crate::devices::appi::Appi;
use crate::devices::appi2::{Appi2, Appi2Variant};
use crate::{error::DeviceError, traits::Device, DeviceId, DeviceInfo};

const APPI_VID: u16 = 0x1992;
const APPI_PID: u16 = 0x1972;
const BCD_APPI2M: u16 = 0x7300;

/// К какому драйверу относится найденное устройство.
#[derive(Clone, Copy)]
enum AppiKind {
    Appi,
    Appi2,
    Appi2M,
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
    /// Сохранённое описание конкретного физического прибора для ленивого open.
    raw: nusb::DeviceInfo,
    kind: AppiKind,
    /// Кеш открытого драйвера (открывается при первом обращении).
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

        // Забираем прежние открытые драйверы, чтобы переиспользовать их для
        // приборов, оставшихся на тех же физических портах.
        let mut old: Vec<DeviceHandle> = std::mem::take(&mut *list);

        let device_list = match nusb::list_devices().wait() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("nusb::list_devices() error: {e}");
                return;
            }
        };

        // Диагностика: материализуем список и считаем, сколько приборов с нашим
        // VID:PID реально видит ОС. Если здесь 1 при двух подключённых — проблема
        // не в коде, а в драйвере/хабе/композитном устройстве на стороне ОС.
        let all: Vec<nusb::DeviceInfo> = device_list.collect();
        let matching = all
            .iter()
            .filter(|d| d.vendor_id() == APPI_VID && d.product_id() == APPI_PID)
            .count();
        tracing::info!(
            "enumerate: {} usb device(s) total, {} with VID:PID {:04X}:{:04X}",
            all.len(),
            matching,
            APPI_VID,
            APPI_PID
        );

        for info in all {
            let info: nusb::DeviceInfo = info;
            if info.vendor_id() != APPI_VID || info.product_id() != APPI_PID {
                continue;
            }

            let bcd_raw = info.device_version();
            let product_str = info.product_string().map(|s: &str| s.to_string());

            // bus + port уникальны для каждого физического прибора, даже когда
            // VID/PID/serial совпадают. По ним и различаем устройства.
            let id_bus = info.device_address();
            let id_port = info.port_number().to_string();
            let id_vid = info.vendor_id();
            let id_pid = info.product_id();
            let id_bus_id = info.bus_id().to_string();
            let manufacturer = info.manufacturer_string().unwrap_or("Appi").to_string();

            let kind = classify(bcd_raw, product_str.as_deref());
            let product_name = match kind {
                AppiKind::Appi2M => Appi2Variant::Appi2M.name().to_string(),
                AppiKind::Appi2 => Appi2Variant::Appi2.name().to_string(),
                AppiKind::Appi => "АППИ".to_string(),
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

            // Если этот же прибор уже был открыт в прошлый раз (тот же bus+port),
            // переносим живой драйвер — не дёргаем USB и не рвём сессию.
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

    /// Возвращает драйвер устройства по индексу, открывая его при первом
    /// обращении (ленивое открытие) и кешируя результат.
    pub async fn get_driver(&self, idx: usize) -> Result<Arc<dyn Device>, DeviceError> {
        // Быстрый путь: драйвер уже открыт.
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
        let device = raw.open().wait().map_err(DeviceError::from)?;
        let driver: Arc<dyn Device> = match h.kind {
            AppiKind::Appi2M => Arc::new(Appi2::open(device, Appi2Variant::Appi2M)?),
            AppiKind::Appi2 => Arc::new(Appi2::open(device, Appi2Variant::Appi2)?),
            AppiKind::Appi => Arc::new(Appi::open(device)?),
        };
        h.driver = Some(driver.clone());
        Ok(driver)
    }

    pub async fn get_info(&self, idx: usize) -> Option<DeviceInfo> {
        self.devices.read().await.get(idx).map(|h| h.info.clone())
    }
}

/// Определяет, какой драйвер использовать для конкретной железки.
///
/// Все устройства с VID:PID 1992:1972 — это семейство АППИ, и по
/// USB-протоколу они общаются одинаково. Различия между моделями
/// (АППИ / АППИ-2 / АППИ-2М) — это вопрос того, какие физические
/// интерфейсы реально выведены наружу (UART/RS-485 могут отсутствовать
/// на самой ранней АППИ), но программно мы это узнаём только по
/// фактическому ответу устройства, а не по дескриптору.
///
/// Поэтому стратегия простая: **по умолчанию выбираем `Appi2`** —
/// полнофункциональный драйвер (CAN + UART + Gen). Если у конкретной
/// железки чего-то нет, мы получим обычную USB-ошибку при попытке
/// использовать, а не отрезаем функционал заранее.
///
/// `Appi2M` распознаётся явно по bcd=0x7300 — это единственный
/// надёжный признак из прошивки, который мы видели на парте устройств.
/// Для других bcd / product мы НЕ пытаемся угадать модель: считаем,
/// что это всё равно АППИ-2 — функционально идентично для нас.
fn classify(bcd_raw: u16, _product: Option<&str>) -> AppiKind {
    if bcd_raw == BCD_APPI2M {
        return AppiKind::Appi2M;
    }
    // Всё остальное — Appi2. Сюда попадают:
    //  - АППИ-2М с прошивкой, где bcd != 0x7300 (видели bcd=0x0000 и 0x0100);
    //  - АППИ-2 со штатной прошивкой;
    //  - теоретическая АППИ-1, если такая физически встретится — у неё
    //    может не работать UART, тогда команда выдаст USB-ошибку, что
    //    честнее, чем заранее скрывать возможность от пользователя.
    AppiKind::Appi2
}