//! Драйвер АППИ-2 / АППИ-2М (VID=0x1992 PID=0x1972).

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use nusb::{Interface, MaybeFuture};

use crate::devices::appi_common as ac;
use crate::error::DeviceError;
use crate::traits::{
    CanCapability, CapabilitySet, Device, GenCapability, UartCapability,
};
use crate::types::{BaudRate, CanFrame, CanInterface, GenSettings, UartMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appi2Variant {
    Appi2,
    Appi2M,
}

impl Appi2Variant {
    pub fn name(self) -> &'static str {
        match self {
            Appi2Variant::Appi2 => "АППИ-2",
            Appi2Variant::Appi2M => "АППИ-2М",
        }
    }
}

#[derive(Default)]
struct Appi2State {
    baud_can1: Option<BaudRate>,
    baud_can2: Option<BaudRate>,
    analyzer_mode_set: bool,
    uart_mode: Option<UartMode>,
    tx_counter: u8,
}

pub struct Appi2 {
    iface: Interface,
    layout: ac::BufferLayout,
    variant: Appi2Variant,
    state: Mutex<Appi2State>,
    // гарантирует, что в каждый момент времени идёт не более одной USB-транзакции
    usb_lock: tokio::sync::Mutex<()>,
}

unsafe impl Send for Appi2 {}
unsafe impl Sync for Appi2 {}

impl Appi2 {
    pub fn open(device: nusb::Device, variant: Appi2Variant) -> Result<Self, DeviceError> {
        #[cfg(target_os = "linux")]
        let iface = device.detach_and_claim_interface(0).wait().map_err(DeviceError::from)?;

        #[cfg(not(target_os = "linux"))]
        let iface = device.claim_interface(1).wait().map_err(DeviceError::from)?;

        Ok(Self {
            iface,
            layout: ac::BufferLayout::APPI,
            variant,
            state: Mutex::new(Appi2State::default()),
            usb_lock: tokio::sync::Mutex::new(()),
        })
    }

    pub fn variant(&self) -> Appi2Variant {
        self.variant
    }

    // внутренние методы — вызываются только под usb_lock

    async fn set_baudrate_inner(&self, iface: CanInterface, baud: BaudRate) -> Result<(), DeviceError> {
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

    async fn ensure_configured_inner(&self) -> Result<(), DeviceError> {
        let need_analyzer = !self.state.lock().unwrap().analyzer_mode_set;
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
            self.set_baudrate_inner(CanInterface::Can1, BaudRate::DEFAULT).await?;
        }
        if need_b2 {
            self.set_baudrate_inner(CanInterface::Can2, BaudRate::DEFAULT).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Device for Appi2 {
    async fn get_version(&self) -> Result<String, DeviceError> {
        let _guard = self.usb_lock.lock().await;
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
        CapabilitySet::all()
    }

    fn as_can(&self)  -> Option<&dyn CanCapability>  { Some(self) }
    fn as_uart(&self) -> Option<&dyn UartCapability> { Some(self) }
    fn as_gen(&self)  -> Option<&dyn GenCapability>  { Some(self) }
}

#[async_trait]
impl CanCapability for Appi2 {
    fn can_interfaces(&self) -> Vec<CanInterface> {
        vec![CanInterface::Can1, CanInterface::Can2]
    }

    async fn set_baudrate(&self, iface: CanInterface, baud: BaudRate) -> Result<(), DeviceError> {
        let _guard = self.usb_lock.lock().await;
        self.set_baudrate_inner(iface, baud).await
    }

    async fn can_read(&self, iface: CanInterface) -> Result<Vec<CanFrame>, DeviceError> {
        let _guard = self.usb_lock.lock().await;
        self.ensure_configured_inner().await?;
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

    async fn can_write(&self, iface: CanInterface, frame: &CanFrame) -> Result<(), DeviceError> {
        let counter = {
            let mut st = self.state.lock().unwrap();
            st.tx_counter = st.tx_counter.wrapping_add(1);
            st.tx_counter
        };
        let pkt = ac::pkt_can_write(iface, frame, counter)?;
        let _guard = self.usb_lock.lock().await;
        ac::usb_write(&self.iface, &pkt).await
    }
}

#[async_trait]
impl UartCapability for Appi2 {
    async fn uart_configure(&self, mode: UartMode) -> Result<(), DeviceError> {
        let pkt = ac::pkt_uart_configure(mode);
        let _guard = self.usb_lock.lock().await;
        ac::usb_write(&self.iface, &pkt).await?;
        self.state.lock().unwrap().uart_mode = Some(mode);
        Ok(())
    }

    async fn uart_write(&self, data: &[u8]) -> Result<(), DeviceError> {
        let configured = self.state.lock().unwrap().uart_mode.is_some();
        if !configured {
            return Err(DeviceError::Protocol(
                "UART не сконфигурирован (вызовите uart_configure)".into(),
            ));
        }
        let pkt = ac::pkt_uart_write(data);
        let _guard = self.usb_lock.lock().await;
        ac::usb_write(&self.iface, &pkt).await
    }

    async fn uart_read(&self) -> Result<Vec<u8>, DeviceError> {
        let _guard = self.usb_lock.lock().await;
        let buf = ac::read_full_buffer(&self.iface, &self.layout).await?;
        if !ac::is_valid_buffer(&buf) {
            return Ok(Vec::new());
        }
        Ok(Vec::new())
    }
}

#[async_trait]
impl GenCapability for Appi2 {
    async fn gen_set(&self, settings: &GenSettings) -> Result<(), DeviceError> {
        let pkt = ac::pkt_gen_set(settings);
        let _guard = self.usb_lock.lock().await;
        ac::usb_write(&self.iface, &pkt).await
    }

    async fn gen_set_frequency(&self, channel: u8, frequency: u32) -> Result<(), DeviceError> {
        let pkt = ac::pkt_gen_frequency(channel, frequency);
        let _guard = self.usb_lock.lock().await;
        ac::usb_write(&self.iface, &pkt).await
    }

    async fn gen_set_amplitude(&self, channel: u8, amplitude: u16) -> Result<(), DeviceError> {
        let pkt = ac::pkt_gen_amplitude(channel, amplitude);
        let _guard = self.usb_lock.lock().await;
        ac::usb_write(&self.iface, &pkt).await
    }
}
