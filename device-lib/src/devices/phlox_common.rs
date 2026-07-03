//! Общая логика протокола Phlox: CRC8, сборка/разбор кадров `ID|len|CRC8|data`,
//! хендшейк модуля Master (Start → Device Info) и разбор списка модулей
//! устройства.
//!
//! Кадр — это НЕ разовый USB-пакет, а элемент непрерывного байтового потока:
//! несколько кадров могут прийти в одной USB-транзакции, а один кадр — быть
//! разрезан на несколько. Поэтому парсинг всегда работает поверх
//! накопительного буфера (см. [`find_frame`]), а не поверх одного `read()`.
//!
//! ВАЖНО (предположение, требует проверки на реальном железе): порядок байт
//! многобайтовых полей (`len`, `TIME`, `Value` и т.д.) в документе не
//! указан явно. Здесь принят big-endian по аналогии с остальными протоколами
//! проекта (appd/appi). Если реальное устройство пришлёт little-endian —
//! поменять `from_be_bytes`/`to_be_bytes` на `_le` в этом файле, слой выше
//! не пострадает.

use crate::error::DeviceError;

// ── модули (ID кадра) ──────────────────────────────────────────────────────

pub const MODULE_MASTER: u8 = 0;
pub const MODULE_CAN1: u8 = 1;
pub const MODULE_CAN2: u8 = 2;
pub const MODULE_CAN3: u8 = 3;
pub const MODULE_CAN4: u8 = 4;
pub const MODULE_CANTECH: u8 = 5;
pub const MODULE_UART1: u8 = 6;
pub const MODULE_UART2: u8 = 7;
pub const MODULE_UART3: u8 = 8;
pub const MODULE_UART4: u8 = 9;
pub const MODULE_GEN1: u8 = 10;
pub const MODULE_GEN2: u8 = 11;
pub const MODULE_ERROR: u8 = 128;

// ── типы сообщений Master ──────────────────────────────────────────────────

pub const MASTER_TYPE_DEVICE_INFO: u8 = 0x01;
pub const MASTER_TYPE_START: u8 = 0x01;

// ── CRC8 ────────────────────────────────────────────────────────────────────

/// CRC-8, poly=0x31, init=0xFF (см. протокол Phlox). `CRC8(b"123456789") == 0xF7`.
pub fn crc8(block: &[u8]) -> u8 {
    let mut crc: u8 = 0xFF;
    for &byte in block {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x31
            } else {
                crc << 1
            };
        }
    }
    crc
}

// ── кадр ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub module_id: u8,
    pub data: Vec<u8>,
}

/// Собирает кадр `ID|len(2, BE)|CRC8|data` для отправки в устройство.
pub fn encode_frame(module_id: u8, data: &[u8]) -> Vec<u8> {
    let len = data.len() as u16;
    let len_le = len.to_le_bytes();
    let header = [module_id, len_le[0], len_le[1]];
    let crc = crc8(&header);

    let mut out = Vec::with_capacity(4 + data.len());
    out.extend_from_slice(&header);
    out.push(crc);
    out.extend_from_slice(data);
    out
}

/// Ищет в буфере первый валидный кадр (по совпадению CRC8 заголовка).
///
/// Возвращает кадр и количество байт буфера, которые он занял вместе с
/// пропущенным "мусором" перед ним (для среза уже обработанного префикса).
/// `None`, если полного валидного кадра в буфере ещё нет — нужно дочитать
/// данные из USB и повторить попытку.
pub fn find_frame(buf: &[u8]) -> Option<(Frame, usize)> {
    if buf.len() < 4 {
        return None;
    }
    for start in 0..=(buf.len() - 4) {
        let header = &buf[start..start + 3];
        let crc = buf[start + 3];
        if crc8(header) != crc {
            continue;
        }
        let len = u16::from_le_bytes([header[1], header[2]]) as usize;
        let end = start + 4 + len;
        if end > buf.len() {
            // Заголовок похож на валидный, но данных ещё не хватает —
            // ждём следующую порцию, не пропуская этот старт.
            return None;
        }
        let frame = Frame {
            module_id: header[0],
            data: buf[start + 4..end].to_vec(),
        };
        return Some((frame, end));
    }
    None
}

// ── Master: Start / Device Info

/// Кадр запроса "Start" в модуль Master.
pub fn start_frame(rid: u8) -> Vec<u8> {
    encode_frame(MODULE_MASTER, &[MASTER_TYPE_START, rid])
}

#[derive(Debug, Clone)]
pub struct ModuleDesc {
    pub id: u8,
    pub kind: u8,
    pub name: String,
    pub params: Vec<(u8, u32)>,
}

#[derive(Debug, Clone)]
pub struct DeviceInfoMsg {
    pub time: u16,
    pub rid: u8,
    pub version: u8,
    pub subversion: u8,
    pub compat_version: u8,
    pub modules: Vec<ModuleDesc>,
}

/// Разбирает "Device info" (Type=0x01) из данных кадра модуля Master.
pub fn parse_device_info(data: &[u8]) -> Result<DeviceInfoMsg, DeviceError> {
    if data.len() < 7 {
        return Err(DeviceError::Protocol("Device Info: кадр короче 7 байт".into()));
    }
    if data[0] != MASTER_TYPE_DEVICE_INFO {
        return Err(DeviceError::Protocol(format!(
            "Master: ожидался Type=0x01 (Device Info), получено 0x{:02X}",
            data[0]
        )));
    }

    let time = u16::from_be_bytes([data[1], data[2]]);
    let rid = data[3];
    let version = data[4];
    let subversion = data[5];
    let compat_version = data[6];

    let mut modules = Vec::new();
    let mut off = 7;
    while off < data.len() {
        if off + 5 > data.len() {
            return Err(DeviceError::Protocol(
                "Device Info: обрезан заголовок записи устройства".into(),
            ));
        }
        let id = data[off];
        let kind = data[off + 1];
        let name_len = u16::from_be_bytes([data[off + 2], data[off + 3]]) as usize;
        let params_count = data[off + 4] as usize;

        let name_start = off + 5;
        let name_end = name_start + name_len;
        if name_end > data.len() {
            return Err(DeviceError::Protocol(
                "Device Info: обрезано имя устройства".into(),
            ));
        }
        let name = decode_cp1251(&data[name_start..name_end]);

        let params_start = name_end;
        let params_end = params_start + params_count * 5;
        if params_end > data.len() {
            return Err(DeviceError::Protocol(
                "Device Info: обрезаны параметры устройства".into(),
            ));
        }
        let mut params = Vec::with_capacity(params_count);
        for i in 0..params_count {
            let p = params_start + i * 5;
            let key = data[p];
            let value = u32::from_be_bytes([data[p + 1], data[p + 2], data[p + 3], data[p + 4]]);
            params.push((key, value));
        }

        modules.push(ModuleDesc {
            id,
            kind,
            name,
            params,
        });
        off = params_end;
    }

    Ok(DeviceInfoMsg {
        time,
        rid,
        version,
        subversion,
        compat_version,
        modules,
    })
}

/// Декодирует байтовую строку CP-1251 в UTF-8.
fn decode_cp1251(bytes: &[u8]) -> String {
    const HIGH: [u16; 64] = [
        0x0402, 0x0403, 0x201A, 0x0453, 0x201E, 0x2026, 0x2020, 0x2021, 0x20AC, 0x2030, 0x0409,
        0x2039, 0x040A, 0x040C, 0x040B, 0x040F, 0x0452, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022,
        0x2013, 0x2014, 0xFFFD, 0x2122, 0x0459, 0x203A, 0x045A, 0x045C, 0x045B, 0x045F, 0x00A0,
        0x040E, 0x045E, 0x0408, 0x00A4, 0x0490, 0x00A6, 0x00A7, 0x0401, 0x00A9, 0x0404, 0x00AB,
        0x00AC, 0x00AD, 0x00AE, 0x0407, 0x00B0, 0x00B1, 0x0406, 0x0456, 0x0491, 0x00B5, 0x00B6,
        0x00B7, 0x0451, 0x2116, 0x0454, 0x00BB, 0x0458, 0x0405, 0x0455, 0x0457,
    ];

    bytes
        .iter()
        .map(|&b| {
            let cp: u32 = match b {
                0x00..=0x7F => b as u32,
                0x80..=0xBF => HIGH[(b - 0x80) as usize] as u32,
                0xC0..=0xDF => 0x0410 + (b - 0xC0) as u32,
                0xE0..=0xFF => 0x0430 + (b - 0xE0) as u32,
            };
            char::from_u32(cp).unwrap_or('\u{FFFD}')
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc8_check_value() {
        assert_eq!(crc8(b"123456789"), 0xF7);
    }

    #[test]
    fn frame_roundtrip() {
        let raw = encode_frame(MODULE_CAN1, &[0xAA, 0xBB, 0xCC]);
        let (frame, consumed) = find_frame(&raw).unwrap();
        assert_eq!(consumed, raw.len());
        assert_eq!(frame.module_id, MODULE_CAN1);
        assert_eq!(frame.data, vec![0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn frame_resync_skips_garbage() {
        let mut buf = vec![0x00, 0x11, 0x22]; // мусор перед кадром
        buf.extend(encode_frame(MODULE_MASTER, &[0x01, 0x05]));
        let (frame, consumed) = find_frame(&buf).unwrap();
        assert_eq!(frame.module_id, MODULE_MASTER);
        assert_eq!(frame.data, vec![0x01, 0x05]);
        assert_eq!(consumed, buf.len());
    }

    #[test]
    fn frame_incomplete_returns_none() {
        let raw = encode_frame(MODULE_CAN1, &[0xAA, 0xBB, 0xCC]);
        assert!(find_frame(&raw[..raw.len() - 1]).is_none());
    }

    #[test]
    fn device_info_parses_module_list() {
        let mut data = vec![MASTER_TYPE_DEVICE_INFO];
        data.extend_from_slice(&100u16.to_be_bytes()); // TIME
        data.push(0x05); // RID
        data.push(2); // Version
        data.push(1); // Subversion
        data.push(1); // Compatible version

        // модуль CAN1, имя "CAN1" (ASCII), без параметров
        data.push(MODULE_CAN1);
        data.push(0x01); // Type = CAN
        data.extend_from_slice(&4u16.to_be_bytes()); // name len
        data.push(0); // params count
        data.extend_from_slice(b"CAN1");

        let info = parse_device_info(&data).unwrap();
        assert_eq!(info.rid, 0x05);
        assert_eq!(info.version, 2);
        assert_eq!(info.modules.len(), 1);
        assert_eq!(info.modules[0].id, MODULE_CAN1);
        assert_eq!(info.modules[0].name, "CAN1");
    }

    #[test]
    fn cp1251_decodes_cyrillic() {
        // "АБВ" в CP-1251: 0xC0 0xC1 0xC2
        assert_eq!(decode_cp1251(&[0xC0, 0xC1, 0xC2]), "АБВ");
    }
}
