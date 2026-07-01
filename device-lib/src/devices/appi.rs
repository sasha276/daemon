//! Драйвер обычной АППИ (VID=0x1992 PID=0x1972, bcdDevice=0x0000).

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use nusb::{Interface, MaybeFuture};

use crate::devices::appi_common as ac;
use crate::error::DeviceError;
use crate::traits::{CanCapability, CapabilitySet, Device};
use crate::types::{BaudRate, CanFrame, CanInterface};

#[derive(Default)]
struct AppiState {
    baud_can1: Option<BaudRate>,
    baud_can2: Option<BaudRate>,
    analyzer_mode_set: bool,
    tx_counter: u8,
}

pub struct Appi {
    iface: Interface,
    layout: ac::BufferLayout,
    state: Mutex<AppiState>,
}

unsafe impl Send for Appi {}
unsafe impl Sync for Appi {}

impl Appi {
    /// Открывает драйвер на УЖЕ ОТКРЫТОМ конкретном устройстве.
    /// `device` создаётся в `DeviceManager::refresh()` из конкретного
    /// `nusb::DeviceInfo`, поэтому при двух одинаковых VID:PID каждый
    /// прибор получает свой собственный хэндл (раньше open искал прибор
    /// заново через list_devices().find() и всегда брал первый).
    pub fn open(device: nusb::Device) -> Result<Self, DeviceError> {

        #[cfg(target_os = "linux")]
        let iface = device.detach_and_claim_interface(0).wait().map_err(DeviceError::from)?;

        #[cfg(not(target_os = "linux"))]
        let iface = device.claim_interface(1).wait().map_err(DeviceError::from)?;

        Ok(Self {
            iface,
            layout: ac::BufferLayout::APPI,
            state: Mutex::new(AppiState::default()),
        })
    }

    async fn ensure_configured(&self) -> Result<(), DeviceError> {
        let need_analyzer = {
            let st = self.state.lock().unwrap();
            !st.analyzer_mode_set
        };
        if need_analyzer {
            ac::usb_write(&self.iface, &ac::pkt_analyzer_mode()).await?;
            tokio::time::sleep(Duration::from_millis(100)).await;
            self.state.lock().unwrap().analyzer_mode_set = true;
        }

        let (need_b1, need_b2) = {
            let st = self.state.lock().unwrap();
            (st.baud_can1.is_none(), st.baud_can2.is_none())
        };
        if need_b1 {
            self.set_baudrate(CanInterface::Can1, BaudRate::DEFAULT).await?;
        }
        if need_b2 {
            self.set_baudrate(CanInterface::Can2, BaudRate::DEFAULT).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Device for Appi {
    async fn get_version(&self) -> Result<String, DeviceError> {
        ac::usb_write(&self.iface, &ac::pkt_version()).await?;
        let mut last_head: Vec<u8> = Vec::new();
        for _ in 0..64 {
            let buf = ac::read_full_buffer(&self.iface, &self.layout).await?;
            if buf[0] == ac::CMD_VERSION {
                return Ok(format!("{}", buf[6]));
            }
            last_head = buf[..buf.len().min(16)].to_vec();
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let hex: String = last_head.iter().map(|b| format!("{b:02X} ")).collect();
        Err(DeviceError::Protocol(format!(
            "версия не распознана; начало буфера: {}",
            hex.trim_end()
        )))
    }

    async fn reset(&self) -> Result<(), DeviceError> {
        Ok(())
    }

    fn capabilities(&self) -> CapabilitySet {
        CapabilitySet::can_only()
    }

    fn as_can(&self) -> Option<&dyn CanCapability> {
        Some(self)
    }
}

#[async_trait]
impl CanCapability for Appi {
    fn can_interfaces(&self) -> Vec<CanInterface> {
        vec![CanInterface::Can1, CanInterface::Can2]
    }

    async fn set_baudrate(
        &self,
        iface: CanInterface,
        baud: BaudRate,
    ) -> Result<(), DeviceError> {
        let pkt = ac::pkt_set_baud(iface, baud)?;
        ac::usb_write(&self.iface, &pkt).await?;
        let mut st = self.state.lock().unwrap();
        match iface {
            CanInterface::Can1 => st.baud_can1 = Some(baud),
            CanInterface::Can2 => st.baud_can2 = Some(baud),
            _ => {}
        }
        Ok(())
    }

    async fn can_read(&self, iface: CanInterface) -> Result<Vec<CanFrame>, DeviceError> {
        self.ensure_configured().await?;

        loop {
            let buf = ac::read_full_buffer(&self.iface, &self.layout).await?;

            if !ac::is_valid_buffer(&buf) || buf[0] == ac::CMD_VERSION {
                tokio::time::sleep(Duration::from_millis(5)).await;
                continue;
            }

            let (new_can1, new_can2) = ac::new_frame_counts(&buf);
            let new_count = match iface {
                CanInterface::Can1 => new_can1,
                CanInterface::Can2 => new_can2,
                _ => 0,
            };

            if new_count == 0 {
                return Ok(Vec::new());
            }

            return ac::parse_frames(&buf, &self.layout, iface, Some(new_count as usize));
        }
    }

    async fn can_write(
        &self,
        iface: CanInterface,
        frame: &CanFrame,
    ) -> Result<(), DeviceError> {
        let counter = {
            let mut st = self.state.lock().unwrap();
            st.tx_counter = st.tx_counter.wrapping_add(1);
            st.tx_counter
        };
        let pkt = ac::pkt_can_write(iface, frame, counter)?;
        ac::usb_write(&self.iface, &pkt).await
    }
}
