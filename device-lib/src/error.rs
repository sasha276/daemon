use thiserror::Error;

#[derive(Debug, Error)]
pub enum DeviceError {
    #[error("USB error: {0}")]
    Usb(String),

    #[error("Device not found")]
    NotFound,

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Timeout")]
    Timeout,

    #[error("No data")]
    NoData,

    #[error("Sync failed")]
    SyncFailed,
}

impl From<nusb::Error> for DeviceError {
    fn from(e: nusb::Error) -> Self {
        DeviceError::Usb(e.to_string())
    }
}

impl From<nusb::transfer::TransferError> for DeviceError {
    fn from(e: nusb::transfer::TransferError) -> Self {
        DeviceError::Usb(e.to_string())
    }
}
