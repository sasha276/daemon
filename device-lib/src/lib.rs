pub mod error;
pub mod traits;
pub mod types;
pub mod device_info;
pub mod manager;
pub mod devices;

pub use device_info::{DeviceId, DeviceInfo};
pub use manager::{DeviceManager, DeviceHandle};
pub use traits::{
    Device, CapabilitySet,
    CanCapability, UartCapability, GenCapability,
};
pub use types::{
    BaudRate, CanFrame, CanInterface,
    UartMode, GenCode, GenSettings,
};
pub use error::DeviceError;
