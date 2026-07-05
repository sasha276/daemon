/// Binary packet layout (all fields big-endian):
///
///  0        1        2        3        4        5        6        7
/// ┌────────────────┬────────────────┬────────────────┬────────────────┐
/// │   client_id    │   session_id   │      seq       │      cmd       │
/// ├────────────────┴────────────────┴────────────────┴────────────────┤
/// │  payload_len   │              payload (N bytes) ...               │
/// └────────────────┴───────────────────────────────────────────────────┘
///
/// Response: cmd | 0x8000
/// Error:    cmd = 0xFFFF, payload = UTF-8 error string
use crate::error::AppError;

pub const HEADER_LEN: usize = 10;

/// Commands (request direction unless noted)
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    // управление клиентами
    /// Register new client → responds with assigned client_id
    Register        = 0x0001,
    /// Unregister client, close all its sessions
    Unregister      = 0x0002,
    /// Ping / keepalive
    Ping            = 0x0003,

    // обнаружение устройств
    /// List connected USB devices
    DeviceList      = 0x0010,
    /// Get info for a specific device by index
    DeviceInfo      = 0x0011,
    /// Get firmware version by device index (no session needed)
    DeviceGetVersion = 0x0012,

    // управление сессиями
    /// Open a session on a device
    SessionCreate   = 0x0020,
    /// Close a session
    SessionClose    = 0x0021,
    /// List active sessions for this client
    SessionList     = 0x0022,

    // команды устройства (требуют session_id)
    /// Get firmware version
    GetVersion      = 0x0030,
    /// Check UART presence
    CheckUart       = 0x0031,

    // CAN-потоковая передача
    /// Open extra UDP stream port for CAN read/write
    /// Payload: data_port:u16 (port the client listens on)
    StreamOpen      = 0x0040,
    /// Close stream port
    StreamClose     = 0x0041,
    /// Single CAN write (no extra port needed)
    CanWrite        = 0x0042,
    /// Periodic CAN write: start
    CanPeriodicStart = 0x0043,
    /// Periodic CAN write: stop
    CanPeriodicStop = 0x0044,
    /// Set CAN baudrate. Payload: iface:u8 + baud:u16 (raw value)
    CanSetBaudrate  = 0x0045,
    /// Single synchronous CAN read. Payload: iface:u8.
    /// Response: count:u16 + repeated hex-line frames (len:u8 + bytes)
    CanReadOnce     = 0x0046,

    // UART
    /// Configure UART mode. Payload: mode:u8 (+ mode-specific bytes)
    UartConfigure   = 0x0060,
    /// Write to UART. Payload: data bytes
    UartWrite       = 0x0061,
    /// Synchronous UART read. Response: received bytes
    UartRead        = 0x0062,
    /// Open UART read stream port. Payload: data_port:u16
    UartStreamOpen  = 0x0063,
    /// Close UART read stream port
    UartStreamClose = 0x0064,

    // генератор сигналов
    /// Full generator setup. Payload: see GenSettings layout below
    GenSetMode      = 0x0070,
    /// Set channel frequency. Payload: channel:u8 + frequency:u32
    GenSetFrequency = 0x0071,
    /// Set channel amplitude. Payload: channel:u8 + amplitude:u16
    GenSetAmplitude = 0x0072,

    // только сервер → клиент
    /// Async CAN frame pushed to stream port
    CanFrame        = 0x0050,
    /// Async event / notification
    Event           = 0x00F0,

    // ответ об ошибке
    Error           = 0xFFFF,
}

impl TryFrom<u16> for Cmd {
    type Error = AppError;
    fn try_from(v: u16) -> Result<Self, AppError> {
        match v {
            0x0001 => Ok(Cmd::Register),
            0x0002 => Ok(Cmd::Unregister),
            0x0003 => Ok(Cmd::Ping),
            0x0010 => Ok(Cmd::DeviceList),
            0x0011 => Ok(Cmd::DeviceInfo),
            0x0012 => Ok(Cmd::DeviceGetVersion),
            0x0020 => Ok(Cmd::SessionCreate),
            0x0021 => Ok(Cmd::SessionClose),
            0x0022 => Ok(Cmd::SessionList),
            0x0030 => Ok(Cmd::GetVersion),
            0x0031 => Ok(Cmd::CheckUart),
            0x0040 => Ok(Cmd::StreamOpen),
            0x0041 => Ok(Cmd::StreamClose),
            0x0042 => Ok(Cmd::CanWrite),
            0x0043 => Ok(Cmd::CanPeriodicStart),
            0x0044 => Ok(Cmd::CanPeriodicStop),
            0x0045 => Ok(Cmd::CanSetBaudrate),
            0x0046 => Ok(Cmd::CanReadOnce),
            0x0060 => Ok(Cmd::UartConfigure),
            0x0061 => Ok(Cmd::UartWrite),
            0x0062 => Ok(Cmd::UartRead),
            0x0063 => Ok(Cmd::UartStreamOpen),
            0x0064 => Ok(Cmd::UartStreamClose),
            0x0070 => Ok(Cmd::GenSetMode),
            0x0071 => Ok(Cmd::GenSetFrequency),
            0x0072 => Ok(Cmd::GenSetAmplitude),
            0x0050 => Ok(Cmd::CanFrame),
            0x00F0 => Ok(Cmd::Event),
            0xFFFF => Ok(Cmd::Error),
            other  => Err(AppError::UnknownCmd(other)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Packet {
    pub client_id:  u16,
    pub session_id: u16,
    pub seq:        u16,
    /// Raw cmd field — may have 0x8000 response flag set
    pub cmd_raw:    u16,
    pub payload:    Vec<u8>,
}

impl Packet {
    pub fn parse(buf: &[u8]) -> Result<Self, AppError> {
        if buf.len() < HEADER_LEN {
            return Err(AppError::PacketTooShort);
        }
        let client_id   = u16::from_be_bytes([buf[0], buf[1]]);
        let session_id  = u16::from_be_bytes([buf[2], buf[3]]);
        let seq         = u16::from_be_bytes([buf[4], buf[5]]);
        let cmd_raw     = u16::from_be_bytes([buf[6], buf[7]]);
        let payload_len = u16::from_be_bytes([buf[8], buf[9]]) as usize;

        if buf.len() < HEADER_LEN + payload_len {
            return Err(AppError::PacketTooShort);
        }
        let payload = buf[HEADER_LEN..HEADER_LEN + payload_len].to_vec();

        Ok(Self { client_id, session_id, seq, cmd_raw, payload })
    }

    pub fn is_response(&self) -> bool { self.cmd_raw & 0x8000 != 0 }
    pub fn base_cmd(&self) -> u16     { self.cmd_raw & 0x7FFF }
    pub fn cmd(&self) -> Result<Cmd, AppError> { Cmd::try_from(self.base_cmd()) }

    // сборка пакетов

    pub fn build_raw(client_id: u16, session_id: u16, seq: u16, cmd: u16, payload: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
        buf.extend_from_slice(&client_id.to_be_bytes());
        buf.extend_from_slice(&session_id.to_be_bytes());
        buf.extend_from_slice(&seq.to_be_bytes());
        buf.extend_from_slice(&cmd.to_be_bytes());
        buf.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        buf.extend_from_slice(payload);
        buf
    }

    /// Normal response — mirrors client_id / session_id / seq from request
    pub fn response(req: &Packet, payload: &[u8]) -> Vec<u8> {
        Self::build_raw(req.client_id, req.session_id, req.seq, 0x8000 | req.cmd_raw, payload)
    }

    /// Error response
    pub fn error(req: &Packet, msg: &str) -> Vec<u8> {
        Self::build_raw(req.client_id, req.session_id, req.seq, 0xFFFF, msg.as_bytes())
    }

    /// Push packet (server-initiated, e.g. CAN frame to stream port)
    pub fn push(client_id: u16, session_id: u16, seq: u16, cmd: Cmd, payload: &[u8]) -> Vec<u8> {
        Self::build_raw(client_id, session_id, seq, cmd as u16, payload)
    }
}

// вспомогательные функции для работы с payload

/// Read u8 from payload at offset
pub fn read_u8(payload: &[u8], offset: usize) -> Result<u8, AppError> {
    payload
        .get(offset)
        .copied()
        .ok_or_else(|| AppError::PayloadDecode(format!("need u8 at offset {offset}")))
}

/// Read u16 from payload at offset
pub fn read_u16(payload: &[u8], offset: usize) -> Result<u16, AppError> {
    if payload.len() < offset + 2 {
        return Err(AppError::PayloadDecode(format!("need u16 at offset {offset}")));
    }
    Ok(u16::from_be_bytes([payload[offset], payload[offset + 1]]))
}

/// Read u32 from payload at offset
pub fn read_u32(payload: &[u8], offset: usize) -> Result<u32, AppError> {
    if payload.len() < offset + 4 {
        return Err(AppError::PayloadDecode(format!("need u32 at offset {offset}")));
    }
    Ok(u32::from_be_bytes([payload[offset], payload[offset+1], payload[offset+2], payload[offset+3]]))
}
