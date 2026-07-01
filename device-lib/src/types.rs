//! Общие типы данных для всех устройств.
//!
//! Эти типы — единый «язык» между драйверами устройств и демоном.
//! Драйвер парсит свой бинарный формат в эти структуры, а обратно —
//! сериализует структуры в формат конкретного железа.

use std::fmt;

// ────────────────────────────────────────────────────────────────────────────
// CAN-интерфейс
// ────────────────────────────────────────────────────────────────────────────

/// Логический CAN-интерфейс устройства.
///
/// Числовые значения совпадают с «выбором интерфейса» из протокола АППИ
/// для команд настройки/чтения на стороне библиотеки (НЕ с байтом 0x02/0x03,
/// который собирается уже внутри драйвера при отправке в железо).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CanInterface {
    Can1 = 1,
    Can2 = 2,
    Can3 = 3,
    Can4 = 4,
    CanTech = 5,
}

impl CanInterface {
    /// Разбор из числа протокола appd (1..=5).
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Can1),
            2 => Some(Self::Can2),
            3 => Some(Self::Can3),
            4 => Some(Self::Can4),
            5 => Some(Self::CanTech),
            _ => None,
        }
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

impl fmt::Display for CanInterface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Can1 => "CAN1",
            Self::Can2 => "CAN2",
            Self::Can3 => "CAN3",
            Self::Can4 => "CAN4",
            Self::CanTech => "CANTECH",
        };
        f.write_str(s)
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Скорость CAN
// ────────────────────────────────────────────────────────────────────────────

/// Скорость шины CAN.
///
/// Хранит «сырое» 16-битное значение в том виде, в котором оно уходит
/// в железо (младший и старший байт по таблице «Настройка CAN»).
/// Например 100 кбит/с = 0x0064, 500 кбит/с = 0x01F4, 1000 кбит/с = 0x03E8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaudRate(pub u16);

impl BaudRate {
    pub const KBPS_25: BaudRate = BaudRate(0x0019);
    pub const KBPS_50: BaudRate = BaudRate(0x0032);
    pub const KBPS_100: BaudRate = BaudRate(0x0064);
    pub const KBPS_250: BaudRate = BaudRate(0x00FA);
    pub const KBPS_500: BaudRate = BaudRate(0x01F4);
    pub const KBPS_1000: BaudRate = BaudRate(0x03E8);

    /// Значение по умолчанию, выставляемое драйвером при инициализации.
    pub const DEFAULT: BaudRate = BaudRate::KBPS_100;

    /// Младший байт (индекс 2 в команде настройки).
    pub fn lo(self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    /// Старший байт (индекс 3 в команде настройки).
    pub fn hi(self) -> u8 {
        (self.0 >> 8) as u8
    }

    /// Сконструировать из «сырого» u16 (как пришло по протоколу appd).
    pub fn from_raw(v: u16) -> Self {
        BaudRate(v)
    }

    pub fn raw(self) -> u16 {
        self.0
    }
}

impl Default for BaudRate {
    fn default() -> Self {
        BaudRate::DEFAULT
    }
}

// ────────────────────────────────────────────────────────────────────────────
// CAN-кадр
// ────────────────────────────────────────────────────────────────────────────

/// Один CAN-кадр в едином виде, независимом от конкретного железа.
///
/// В буфере АППИ кадр лежит как 10 байт: дескриптор (2 байта, big-endian)
/// + 8 байт данных. Дескриптор упакован так:
///   `descriptor = (id << 5) | (size & 0x1F)`
/// где `id` — 11-битный идентификатор, `size` (DLC) — длина данных 0..=8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanFrame {
    /// CAN-идентификатор (11 бит для стандартного кадра).
    pub id: u16,
    /// Длина значимых данных, 0..=8 (DLC).
    pub size: u8,
    /// Данные кадра, всегда дополнены нулями до 8 байт.
    pub data: [u8; 8],
}

impl CanFrame {
    /// Создать кадр из id и среза данных. Данные обрезаются/паддятся до 8 байт.
    pub fn new(id: u16, data: &[u8]) -> Self {
        let mut buf = [0u8; 8];
        let n = data.len().min(8);
        buf[..n].copy_from_slice(&data[..n]);
        CanFrame {
            id,
            size: n as u8,
            data: buf,
        }
    }

    /// Упакованный 16-битный дескриптор: `(id << 5) | (size & 0x1F)`.
    pub fn descriptor(&self) -> u16 {
        (self.id << 5) | (self.size as u16 & 0x1F)
    }

    /// Разобрать кадр из дескриптора и 8 байт данных.
    ///
    /// `id = descriptor >> 5`, `size = descriptor & 0x0F`.
    /// (Размер берётся из младших 4 бит — как в эталонном C#-парсере;
    /// 5-й бит дескриптора зарезервирован под служебный флаг.)
    pub fn from_descriptor(descriptor: u16, data: [u8; 8]) -> Self {
        CanFrame {
            id: descriptor >> 5,
            size: (descriptor & 0x0F) as u8,
            data,
        }
    }

    /// Разобрать кадр из 10-байтового слота буфера (big-endian дескриптор).
    ///
    /// Возвращает `None`, если слот пустой (все 10 байт равны нулю).
    pub fn from_slot_be(slot: &[u8; 10]) -> Option<Self> {
        if slot.iter().all(|&b| b == 0) {
            return None;
        }
        let descriptor = u16::from_be_bytes([slot[0], slot[1]]);
        let mut data = [0u8; 8];
        data.copy_from_slice(&slot[2..10]);
        Some(Self::from_descriptor(descriptor, data))
    }

    /// Собрать 10-байтовый слот буфера (big-endian дескриптор) для отправки.
    pub fn to_slot_be(&self) -> [u8; 10] {
        let desc = self.descriptor().to_be_bytes();
        let mut slot = [0u8; 10];
        slot[0] = desc[0];
        slot[1] = desc[1];
        slot[2..10].copy_from_slice(&self.data);
        slot
    }

    /// Текстовое hex-представление кадра для UDP-стрима.
    ///
    /// Формат (Вариант A): сырой дескриптор 4 hex-символа, пробел,
    /// затем 8 байт данных 16 hex-символов.
    /// Пример: `"3988 FFFFFF21E40A0000"`.
    pub fn to_hex_line(&self) -> String {
        let mut s = String::with_capacity(4 + 1 + 16);
        s.push_str(&format!("{:04X} ", self.descriptor()));
        for b in &self.data {
            s.push_str(&format!("{:02X}", b));
        }
        s
    }
}

impl fmt::Display for CanFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex_line())
    }
}

// ────────────────────────────────────────────────────────────────────────────
// UART
// ────────────────────────────────────────────────────────────────────────────

/// Режим работы UART (по таблице «Настройка UART» протокола АППИ, индекс 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UartMode {
    /// RSDD (MODBUS_RTU).
    Rsdd = 0x01,
    Rs232 = 0x02,
    Rs485 = 0x03,
    Off = 0x09,
}

impl UartMode {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::Rsdd),
            0x02 => Some(Self::Rs232),
            0x03 => Some(Self::Rs485),
            0x09 => Some(Self::Off),
            _ => None,
        }
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Генератор аналоговых сигналов
// ────────────────────────────────────────────────────────────────────────────

/// Режим формирования кода генератора (индекс 5 в «Настройка Генератора»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GenCode {
    Als = 0x01,
    AlsEn = 0x02,
    SautNsp = 0x03,
}

impl GenCode {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::Als),
            0x02 => Some(Self::AlsEn),
            0x03 => Some(Self::SautNsp),
            _ => None,
        }
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Параметры одной настройки генератора.
///
/// Поля соответствуют таблице «Настройка Генератора». Драйвер сам
/// раскладывает их по индексам буфера и заполняет резервы нулями.
#[derive(Debug, Clone)]
pub struct GenSettings {
    /// Формирование кода (индекс 5).
    pub code: GenCode,
    /// Усиление в 10 раз (индекс 6): false=выкл, true=вкл.
    pub gain_x10: bool,
    /// Установка ЦАП, 0..=1.25 В (индексы 7-8).
    pub dac: u16,
    /// АЛСН, настройка кода (индекс 9). 0 если не применяется.
    pub alsn_code: u8,
    /// Установка частоты (индексы 10-13).
    pub frequency: u32,
    /// АЛС-ЕН: Синхронная Группа и Кодовая Комбинация (индексы 14-15).
    pub alsen_sg_kk: u16,
    /// САУТ/НСП, установка частоты (индекс 16). 0 если не применяется.
    pub saut_freq: u8,
    /// САУТ/НСП, блок 0 и блок 1 (индексы 17-22), до 6 байт.
    pub saut_blocks: [u8; 6],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_roundtrip() {
        // id=0x01CC (460), size=8 -> descriptor = (0x01CC << 5) | 8 = 0x3988
        let f = CanFrame::new(0x01CC, &[0xFF, 0xFF, 0xFF, 0x21, 0xE4, 0x0A, 0x00, 0x00]);
        assert_eq!(f.descriptor(), 0x3988);
        assert_eq!(f.size, 8);

        let back = CanFrame::from_descriptor(0x3988, f.data);
        assert_eq!(back.id, 0x01CC);
        assert_eq!(back.size, 8);
    }

    #[test]
    fn hex_line_format() {
        let f = CanFrame::new(0x01CC, &[0xFF, 0xFF, 0xFF, 0x21, 0xE4, 0x0A, 0x00, 0x00]);
        assert_eq!(f.to_hex_line(), "3988 FFFFFF21E40A0000");
    }

    #[test]
    fn slot_be_roundtrip() {
        let f = CanFrame::new(0x123, &[1, 2, 3, 4]);
        let slot = f.to_slot_be();
        let back = CanFrame::from_slot_be(&slot).unwrap();
        assert_eq!(back.id, f.id);
        assert_eq!(back.size, f.size);
        assert_eq!(back.data, f.data);
    }

    #[test]
    fn empty_slot_is_none() {
        let slot = [0u8; 10];
        assert!(CanFrame::from_slot_be(&slot).is_none());
    }

    #[test]
    fn id_zero_dlc_zero_not_empty_when_data_present() {
        // id=0, size=0, но данные есть -> слот НЕ пустой
        let mut slot = [0u8; 10];
        slot[5] = 0xAB;
        assert!(CanFrame::from_slot_be(&slot).is_some());
    }

    #[test]
    fn baud_bytes() {
        assert_eq!(BaudRate::KBPS_100.lo(), 0x64);
        assert_eq!(BaudRate::KBPS_100.hi(), 0x00);
        assert_eq!(BaudRate::KBPS_500.lo(), 0xF4);
        assert_eq!(BaudRate::KBPS_500.hi(), 0x01);
        assert_eq!(BaudRate::KBPS_1000.lo(), 0xE8);
        assert_eq!(BaudRate::KBPS_1000.hi(), 0x03);
    }
}
