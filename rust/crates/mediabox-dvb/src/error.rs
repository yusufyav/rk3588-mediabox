use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    /// libusb said no: its own error name and code.
    #[error("USB: {0}")]
    Usb(#[from] crate::usb::UsbError),
    /// The bridge answered a command with a non-zero status.
    #[error("device error status 0x{status:02X} (cmd 0x{cmd:02X})")]
    Device { status: u8, cmd: u8 },
    /// A reply that is not the frame the protocol defines.
    #[error("protocol: {0}")]
    Protocol(String),
    /// A write outside the approved scope; nothing was sent.
    #[error("policy: {0}")]
    Policy(String),
    #[error("GPIO verify: {0}")]
    GpioVerify(String),
    #[error("demod: {0}")]
    Demod(String),
    #[error("patch: {0}")]
    Patch(String),
    #[error("LNB: {0}")]
    Lnb(String),
    #[error("firmware: {0}")]
    Firmware(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// The device went away under the command (unplugged).
    pub fn is_no_device(&self) -> bool {
        matches!(self, Error::Usb(error) if error.is_no_device())
    }
}
