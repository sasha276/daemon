#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId {
    pub vid:  u16,
    pub pid:  u16,
    pub bus:  u8,
    pub port: String,
}

impl DeviceId {
    pub fn key(&self) -> String {
        format!("{:04X}:{:04X}@{}.{}", self.vid, self.pid, self.bus, self.port)
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub id:           DeviceId,
    pub manufacturer: String,
    pub product:      String,
}
