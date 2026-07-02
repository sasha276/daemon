//! Драйвер устройства Phlox (VID=0x16C0 PID=0x05DC).
//!
//! В отличие от семейства АППИ (см. `appi_common.rs`), Phlox общается
//! потоком кадров `ID|len|CRC8|data` (см. `phlox_common.rs`), а не сырыми
//! пакетами фиксированного размера. Набор модулей (CAN/UART/Gen) устройство
//! сообщает само в ответе "Device Info" — здесь их количество не фиксировано
//! заранее, как у АППИ, поэтому `CanCapability`/`UartCapability`/
//! `GenCapability` для Phlox пока не реализованы (см. README задачи):
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
use nusb::{Interface};

use crate::devices::phlox_common as pc;
use crate::error::DeviceError;
use crate::traits::{CapabilitySet, Device};

/// Интерфейс USB, который пробуем захватить по умолчанию.
///
/// Предположение до проверки на реальном железе: у Phlox одна bulk-пара
/// эндпоинтов на интерфейсе 0. Если устройство композитное (несколько
/// интерфейсов), стоит скорректировать при первом реальном подключении.
const DEFAULT_INTERFACE: u8 = 0;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const READ_CHUNK: usize = 512;

#[derive(Default)]
struct PhloxState {
    /// Накопительный буфер входящего потока — кадр может быть разрезан
    /// USB-транзакциями или несколько кадров прийти в одной.
    rx_buf: Vec<u8>,
    next_rid: u8,
}

pub struct Phlox {
    iface: Interface,
    write_ep: u8,
    read_ep: u8,
    state: Mutex<PhloxState>,
}

unsafe impl Send for Phlox {}
unsafe impl Sync for Phlox {}

impl Phlox {
    /// Открывает драйвер на уже открытом устройстве `nusb::Device`.
    pub async fn open(device: nusb::Device) -> Result<Self, DeviceError> {
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

        tracing::info!("Phlox: интерфейс {DEFAULT_INTERFACE} захвачен, ищу bulk-эндпоинты");

        let (write_ep, read_ep) = discover_bulk_endpoints(&iface)?;
        tracing::info!("Phlox: интерфейс {DEFAULT_INTERFACE}, OUT=0x{write_ep:02X} IN=0x{read_ep:02X}");

        Ok(Self {
            iface,
            write_ep,
            read_ep,
            state: Mutex::new(PhloxState::default()),
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

    /// Читает и возвращает следующий валидный кадр из потока, ожидая новые
    /// данные из USB, пока в накопленном буфере нет полного кадра.
    async fn read_frame(&self, deadline: Instant) -> Result<pc::Frame, DeviceError> {
        loop {
            {
                let mut st = self.state.lock().unwrap();
                if let Some((frame, consumed)) = pc::find_frame(&st.rx_buf) {
                    st.rx_buf.drain(..consumed);
                    return Ok(frame);
                }
            }
            if Instant::now() >= deadline {
                return Err(DeviceError::Timeout);
            }
            let chunk = self.usb_read_chunk().await?;
            if chunk.is_empty() {
                continue;
            }
            self.state.lock().unwrap().rx_buf.extend_from_slice(&chunk);
        }
    }

    /// Рукопожатие Master: отправляет Start(rid) и ждёт Device Info с тем же
    /// rid, отбрасывая по пути любые другие кадры (например, от CAN/UART,
    /// если устройство уже что-то принимает).
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
                Ok(_) => continue, // Device Info от другого запроса — не наш rid
                Err(_) => continue, // не Device Info (другой Type) — пропускаем
            }
        }
    }
}

#[async_trait]
impl Device for Phlox {
    async fn get_version(&self) -> Result<String, DeviceError> {
        let info = self.handshake().await?;
        tracing::info!(
            "Phlox: версия {}.{} (совместимость с {}), модулей: {}",
            info.version,
            info.subversion,
            info.compat_version,
            info.modules.len()
        );
        for m in &info.modules {
            tracing::info!("Phlox: модуль id={} type={} name={:?}", m.id, m.kind, m.name);
        }
        Ok(format!("{}.{}", info.version, info.subversion))
    }

    async fn reset(&self) -> Result<(), DeviceError> {
        self.handshake().await?;
        Ok(())
    }

    fn capabilities(&self) -> CapabilitySet {
        // MVP: только версия/хендшейк. CAN/UART/Gen для Phlox добавляются
        // отдельно, когда согласован разбор их сообщений.
        CapabilitySet::NONE
    }
}

/// Определяет bulk IN/OUT эндпоинты активного alt-setting интерфейса.
///
/// VID:PID Phlox не даёт готовых констант эндпоинтов (в отличие от АППИ),
/// поэтому ищем их в дескрипторе интерфейса вместо того, чтобы угадывать.
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
            Direction::In if in_ep.is_none() => in_ep = Some(ep.address()),
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
