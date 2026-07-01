//! Общая логика семейства АППИ: USB IO, парсинг буфера приёма, сборка
//! пакетов CAN/UART/генератора. Используется драйверами `Appi`, `Appi2`,
//! `Appi2M`, отличающимися лишь набором возможностей и параметрами буфера.

use std::time::Duration;

use nusb::Interface;
use nusb::transfer::{Bulk, Buffer, In, Out};

use crate::error::DeviceError;
use crate::types::{BaudRate, CanFrame, CanInterface, GenSettings, UartMode};

pub const TIMEOUT: Duration = Duration::from_millis(500);
pub const WRITE_EP: u8 = 0x01;
pub const READ_EP: u8 = 0x82;

/// Размер одного слота CAN-кадра: дескриптор(2) + данные(8).
pub const SLOT_SIZE: usize = 10;

/// Код команды «настройка интерфейса / запрос версии» (служебка, индекс 0).
pub const CMD_CONFIG: u8 = 0x01;
/// Код команды «отправка данных в интерфейс» (служебка, индекс 0).
pub const CMD_DATA: u8 = 0x02;
/// Код запроса версии (индекс 0 = 0x09).
pub const CMD_VERSION: u8 = 0x09;

/// Параметры раскладки буфера приёма для конкретной модели.
#[derive(Debug, Clone, Copy)]
pub struct BufferLayout {
    /// Полный размер буфера приёма по USB, байт.
    pub total: usize,
    /// Смещение начала буфера CAN1 (CAN_A).
    pub can1_start: usize,
    /// Смещение начала буфера CAN2 (CAN_B).
    pub can2_start: usize,
    /// Количество слотов на один CAN-интерфейс.
    pub slots: usize,
}

impl BufferLayout {
    pub const APPI: BufferLayout = BufferLayout {
        total: 2048,
        can1_start: 24,
        can2_start: 524,
        slots: 45,
    };

    pub const BS_KPA: BufferLayout = BufferLayout {
        total: 2048,
        can1_start: 24,
        can2_start: 224,
        slots: 20,
    };

    fn start_for(&self, iface: CanInterface) -> Option<usize> {
        match iface {
            CanInterface::Can1 => Some(self.can1_start),
            CanInterface::Can2 => Some(self.can2_start),
            _ => None,
        }
    }
}

/// Перевод логического CAN-интерфейса в байт «выбора интерфейса» железа.
pub fn iface_byte(iface: CanInterface) -> Option<u8> {
    match iface {
        CanInterface::Can1 => Some(0x02),
        CanInterface::Can2 => Some(0x03),
        CanInterface::CanTech => Some(0x13),
        CanInterface::Can3 => Some(0x22),
        CanInterface::Can4 => Some(0x23),
    }
}

// низкоуровневый USB IO

pub async fn usb_write(iface: &Interface, data: &[u8]) -> Result<(), DeviceError> {
    let mut ep = iface.endpoint::<Bulk, Out>(WRITE_EP)
        .map_err(|e| DeviceError::Usb(e.to_string()))?;
    ep.submit(data.to_vec().into());
    ep.next_complete().await
        .into_result()
        .map_err(DeviceError::from)?;
    Ok(())
}

pub async fn read_full_buffer(
    iface: &Interface,
    layout: &BufferLayout,
) -> Result<Vec<u8>, DeviceError> {
    let mut ep = iface.endpoint::<Bulk, In>(READ_EP)
        .map_err(|e| DeviceError::Usb(e.to_string()))?;
    let mut result = vec![0u8; layout.total];
    let mut taken = 0;
    while taken < layout.total {
        ep.submit(Buffer::new(layout.total - taken));
        let data = ep.next_complete().await
            .into_result()
            .map_err(DeviceError::from)?;
        let n = data.len();
        if n == 0 {
            return Err(DeviceError::NoData);
        }
        result[taken..taken + n].copy_from_slice(&data);
        taken += n;
    }
    Ok(result)
}

// парсинг буфера приёма

pub fn is_valid_buffer(buf: &[u8]) -> bool {
    !buf.is_empty() && (buf[0] == CMD_CONFIG || buf[0] == CMD_DATA || buf[0] == CMD_VERSION)
}

pub fn new_frame_counts(buf: &[u8]) -> (u8, u8) {
    let can1 = buf.get(6).copied().unwrap_or(0);
    let can2 = buf.get(2).copied().unwrap_or(0);
    (can1, can2)
}

pub fn parse_frames(
    buf: &[u8],
    layout: &BufferLayout,
    iface: CanInterface,
    limit: Option<usize>,
) -> Result<Vec<CanFrame>, DeviceError> {
    let start = layout
        .start_for(iface)
        .ok_or_else(|| DeviceError::Protocol(format!("интерфейс {iface} не в буфере")))?;

    let count = limit.unwrap_or(layout.slots).min(layout.slots);
    let mut frames = Vec::new();
    for i in 0..count {
        let off = start + i * SLOT_SIZE;
        if off + SLOT_SIZE > buf.len() {
            break;
        }
        let mut slot = [0u8; SLOT_SIZE];
        slot.copy_from_slice(&buf[off..off + SLOT_SIZE]);
        if let Some(frame) = CanFrame::from_slot_be(&slot) {
            frames.push(frame);
        }
    }
    Ok(frames)
}

// сборка пакетов на отправку

pub fn pkt_analyzer_mode() -> [u8; 4] {
    [0x04, 0x01, 0x01, 0x00]
}

pub fn pkt_version() -> [u8; 2] {
    [0x09, 0x01]
}

pub fn pkt_set_baud(iface: CanInterface, baud: BaudRate) -> Result<[u8; 4], DeviceError> {
    let ib = iface_byte(iface)
        .ok_or_else(|| DeviceError::Protocol(format!("интерфейс {iface} без байта")))?;
    Ok([ib, CMD_CONFIG, baud.lo(), baud.hi()])
}

pub fn pkt_can_write(
    iface: CanInterface,
    frame: &CanFrame,
    counter: u8,
) -> Result<Vec<u8>, DeviceError> {
    let ib = iface_byte(iface)
        .ok_or_else(|| DeviceError::Protocol(format!("интерфейс {iface} без байта")))?;
    let slot = frame.to_slot_be();
    let mut pkt = vec![0u8; 10 + SLOT_SIZE];
    pkt[0] = ib;
    pkt[1] = CMD_DATA;
    pkt[2] = counter;
    pkt[3] = 0x01;
    pkt[10..10 + SLOT_SIZE].copy_from_slice(&slot);
    Ok(pkt)
}

pub fn pkt_uart_configure(mode: UartMode) -> [u8; 4] {
    [0x01, CMD_CONFIG, mode.as_u8(), 0x00]
}

pub fn pkt_uart_write(data: &[u8]) -> Vec<u8> {
    let mut pkt = vec![0u8; 10 + data.len()];
    pkt[0] = 0x01;
    pkt[1] = CMD_DATA;
    pkt[2] = 0x00;
    let len = data.len() as u16;
    pkt[8] = (len >> 8) as u8;
    pkt[9] = (len & 0xFF) as u8;
    pkt[10..].copy_from_slice(data);
    pkt
}

pub fn pkt_gen_set(s: &GenSettings) -> Vec<u8> {
    let mut pkt = vec![0u8; 23];
    pkt[0] = 0x05;
    pkt[1] = CMD_DATA;
    pkt[5] = s.code.as_u8();
    pkt[6] = s.gain_x10 as u8;
    pkt[7] = (s.dac >> 8) as u8;
    pkt[8] = (s.dac & 0xFF) as u8;
    pkt[9] = s.alsn_code;
    pkt[10..14].copy_from_slice(&s.frequency.to_be_bytes());
    pkt[14] = (s.alsen_sg_kk >> 8) as u8;
    pkt[15] = (s.alsen_sg_kk & 0xFF) as u8;
    pkt[16] = s.saut_freq;
    pkt[17..23].copy_from_slice(&s.saut_blocks);
    pkt
}

pub fn pkt_gen_frequency(channel: u8, frequency: u32) -> Vec<u8> {
    let mut pkt = vec![0u8; 6];
    pkt[0] = 0x02;
    pkt[1] = channel;
    pkt[2..6].copy_from_slice(&frequency.to_be_bytes());
    pkt
}

pub fn pkt_gen_amplitude(channel: u8, amplitude: u16) -> Vec<u8> {
    let mut pkt = vec![0u8; 4];
    pkt[0] = 0x03;
    pkt[1] = channel;
    pkt[2] = (amplitude >> 8) as u8;
    pkt[3] = (amplitude & 0xFF) as u8;
    pkt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GenCode;

    #[test]
    fn baud_packet() {
        let p = pkt_set_baud(CanInterface::Can1, BaudRate::KBPS_100).unwrap();
        assert_eq!(p, [0x02, 0x01, 0x64, 0x00]);
        let p2 = pkt_set_baud(CanInterface::Can2, BaudRate::KBPS_500).unwrap();
        assert_eq!(p2, [0x03, 0x01, 0xF4, 0x01]);
    }

    #[test]
    fn can_write_packet_layout() {
        let f = CanFrame::new(0x01CC, &[0xFF, 0xFF, 0xFF, 0x21, 0xE4, 0x0A, 0x00, 0x00]);
        let p = pkt_can_write(CanInterface::Can1, &f, 7).unwrap();
        assert_eq!(p[0], 0x02);
        assert_eq!(p[1], 0x02);
        assert_eq!(p[2], 7);
        assert_eq!(p[3], 0x01);
        assert_eq!(p[10], 0x39);
        assert_eq!(p[11], 0x88);
        assert_eq!(&p[12..20], &[0xFF, 0xFF, 0xFF, 0x21, 0xE4, 0x0A, 0x00, 0x00]);
    }

    #[test]
    fn parse_two_frames_appi() {
        let layout = BufferLayout::APPI;
        let mut buf = vec![0u8; layout.total];
        buf[0] = CMD_DATA;
        let f0 = CanFrame::new(0x123, &[0xAA, 0xBB]);
        let s0 = f0.to_slot_be();
        let base = layout.can1_start;
        buf[base..base + SLOT_SIZE].copy_from_slice(&s0);
        let f2 = CanFrame::new(0x7FF, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let s2 = f2.to_slot_be();
        let off2 = base + 2 * SLOT_SIZE;
        buf[off2..off2 + SLOT_SIZE].copy_from_slice(&s2);
        let frames = parse_frames(&buf, &layout, CanInterface::Can1, None).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].id, 0x123);
        assert_eq!(frames[0].size, 2);
        assert_eq!(frames[1].id, 0x7FF);
        assert_eq!(frames[1].size, 8);
    }

    #[test]
    fn parse_respects_limit() {
        let layout = BufferLayout::APPI;
        let mut buf = vec![0u8; layout.total];
        buf[0] = CMD_DATA;
        let base = layout.can1_start;
        for i in 0..5 {
            let f = CanFrame::new(0x100 + i as u16, &[i as u8]);
            let s = f.to_slot_be();
            let off = base + i * SLOT_SIZE;
            buf[off..off + SLOT_SIZE].copy_from_slice(&s);
        }
        let frames = parse_frames(&buf, &layout, CanInterface::Can1, Some(3)).unwrap();
        assert_eq!(frames.len(), 3);
    }

    #[test]
    fn gen_packet_indices() {
        let s = GenSettings {
            code: GenCode::Als,
            gain_x10: true,
            dac: 0x0102,
            alsn_code: 0x03,
            frequency: 0x0A0B0C0D,
            alsen_sg_kk: 0x1122,
            saut_freq: 0x02,
            saut_blocks: [1, 2, 3, 4, 5, 6],
        };
        let p = pkt_gen_set(&s);
        assert_eq!(p[0], 0x05);
        assert_eq!(p[1], 0x02);
        assert_eq!(p[5], 0x01);
        assert_eq!(p[6], 0x01);
        assert_eq!(p[7], 0x01);
        assert_eq!(p[8], 0x02);
        assert_eq!(p[9], 0x03);
        assert_eq!(&p[10..14], &[0x0A, 0x0B, 0x0C, 0x0D]);
        assert_eq!(p[14], 0x11);
        assert_eq!(p[15], 0x22);
        assert_eq!(p[16], 0x02);
        assert_eq!(&p[17..23], &[1, 2, 3, 4, 5, 6]);
    }
}
