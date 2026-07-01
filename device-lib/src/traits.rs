//! Трейты устройств, разбитые по «возможностям» (capabilities).
//!
//! Базовый трейт [`Device`] есть у всех. Дополнительные возможности
//! (CAN/UART/генератор) выражены отдельными трейтами. Конкретное устройство
//! реализует базовый `Device` плюс те capability-трейты, которые поддерживает.
//!
//! Демон узнаёт о возможностях устройства через методы `as_can()`/`as_uart()`/
//! `as_gen()` на `Device`: они возвращают `Option<&dyn _Capability>`.

use async_trait::async_trait;

use crate::error::DeviceError;
use crate::types::{BaudRate, CanFrame, CanInterface, GenSettings, UartMode};

// ────────────────────────────────────────────────────────────────────────────
// Базовый трейт
// ────────────────────────────────────────────────────────────────────────────

/// Базовые операции, поддерживаемые любым устройством.
#[async_trait]
pub trait Device: Send + Sync {
    /// Версия ПО устройства (человекочитаемая строка).
    async fn get_version(&self) -> Result<String, DeviceError>;

    /// Сброс устройства к настройкам по умолчанию и очистка очередей.
    async fn reset(&self) -> Result<(), DeviceError>;

    /// Возможности, которыми обладает устройство.
    fn capabilities(&self) -> CapabilitySet;

    // ── доступ к capability-трейтам ───────────────────────────────────────
    // Реализация по умолчанию: возможность отсутствует. Устройство, которое
    // её поддерживает, переопределяет нужный метод, возвращая `Some(self)`.

    fn as_can(&self) -> Option<&dyn CanCapability> {
        None
    }
    fn as_uart(&self) -> Option<&dyn UartCapability> {
        None
    }
    fn as_gen(&self) -> Option<&dyn GenCapability> {
        None
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Набор возможностей
// ────────────────────────────────────────────────────────────────────────────

/// Битовый набор поддерживаемых возможностей устройства.
///
/// Используется демоном, чтобы отдать UI список того, что устройство умеет,
/// без попыток вызывать неподдерживаемые методы.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CapabilitySet {
    pub can: bool,
    pub uart: bool,
    pub gen: bool,
}

impl CapabilitySet {
    pub const NONE: CapabilitySet = CapabilitySet {
        can: false,
        uart: false,
        gen: false,
    };

    pub const fn can_only() -> Self {
        CapabilitySet {
            can: true,
            uart: false,
            gen: false,
        }
    }

    pub const fn all() -> Self {
        CapabilitySet {
            can: true,
            uart: true,
            gen: true,
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// CAN
// ────────────────────────────────────────────────────────────────────────────

/// Возможность работы с CAN-интерфейсами.
#[async_trait]
pub trait CanCapability: Send + Sync {
    /// Список доступных CAN-интерфейсов устройства.
    fn can_interfaces(&self) -> Vec<CanInterface>;

    /// Выставить скорость на интерфейсе. Состояние запоминается драйвером;
    /// последующие чтения НЕ переконфигурируют скорость.
    async fn set_baudrate(&self, iface: CanInterface, baud: BaudRate)
        -> Result<(), DeviceError>;

    /// Прочитать порцию входящих кадров с интерфейса.
    ///
    /// Возвращает только непустые слоты буфера (см. [`CanFrame::from_slot_be`]).
    /// Пустой `Vec` означает «новых кадров нет» — это не ошибка.
    async fn can_read(&self, iface: CanInterface) -> Result<Vec<CanFrame>, DeviceError>;

    /// Отправить кадр в CAN. Драйвер сам собирает бинарный формат железа
    /// из [`CanFrame`].
    async fn can_write(&self, iface: CanInterface, frame: &CanFrame)
        -> Result<(), DeviceError>;
}

// ────────────────────────────────────────────────────────────────────────────
// UART
// ────────────────────────────────────────────────────────────────────────────

/// Возможность работы с UART/RS232/RS485.
#[async_trait]
pub trait UartCapability: Send + Sync {
    /// Настроить режим работы линии.
    async fn uart_configure(&self, mode: UartMode) -> Result<(), DeviceError>;

    /// Отправить данные в текущем режиме.
    async fn uart_write(&self, data: &[u8]) -> Result<(), DeviceError>;

    /// Прочитать накопленные принятые данные. Пустой `Vec` — данных нет.
    async fn uart_read(&self) -> Result<Vec<u8>, DeviceError>;
}

// ────────────────────────────────────────────────────────────────────────────
// Генератор аналоговых сигналов
// ────────────────────────────────────────────────────────────────────────────

/// Возможность управления генератором аналоговых сигналов.
#[async_trait]
pub trait GenCapability: Send + Sync {
    /// Применить полную настройку генератора.
    async fn gen_set(&self, settings: &GenSettings) -> Result<(), DeviceError>;

    /// Изменить только частоту канала (мГц).
    async fn gen_set_frequency(&self, channel: u8, frequency: u32)
        -> Result<(), DeviceError>;

    /// Изменить только амплитуду канала (мВ).
    async fn gen_set_amplitude(&self, channel: u8, amplitude: u16)
        -> Result<(), DeviceError>;
}
