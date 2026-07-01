use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Packet too short")]
    PacketTooShort,

    #[error("Unknown command: 0x{0:04X}")]
    UnknownCmd(u16),

    #[error("Unknown client id: {0}")]
    UnknownClient(u16),

    #[error("Unknown session id: {0}")]
    UnknownSession(u16),

    #[error("Session already exists: {0}")]
    SessionExists(u16),

    #[error("No USB device found at index {0}")]
    DeviceNotFound(usize),

    #[error("USB error: {0}")]
    Usb(String),

    #[error("Payload decode error: {0}")]
    PayloadDecode(String),

    #[error("Internal channel error")]
    ChannelClosed,
}
