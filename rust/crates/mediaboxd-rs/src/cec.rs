//! Which CEC adapter a command is sent down.
//!
//! Every adapter on the board is opened and listened to -- the remote's keys
//! arrive on whichever socket the television is on, and receiving from all of
//! them is harmless. Sending is not. A board with two HDMI transmitters and two
//! televisions has two live adapters, and "the first one with an address" is
//! whichever television happens to be on the lower-numbered socket, not the one
//! the picture is on.
//!
//! So a command goes where the picture goes:
//!
//! ```text
//! selected output -> its transmitter -> that transmitter's CEC adapter
//! ```
//!
//! resolved by `mediabox-platform` with a confidence, and refused -- with the
//! reason, never with a quiet fallback to another adapter -- when the chain is
//! ambiguous, when the adapter has no physical address or logical address, or
//! when the address it holds is not the one the selected sink's EDID declares.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mediabox_platform::{Confidence, Platform};

/// Where the next command should go, as the platform resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CecTarget {
    Adapter {
        path: PathBuf,
        /// The physical address the selected sink's EDID declares, when it
        /// declares one; the adapter must hold the same.
        expected_address: Option<u16>,
        confidence: Confidence,
    },
    Refused(String),
}

/// The target for the output this box is on.
pub fn target(platform: &Platform) -> CecTarget {
    let Some(output) = platform.selected_output() else {
        return CecTarget::Refused("seçili bir ekran çıkışı yok".into());
    };
    let Some(adapter) = &output.cec else {
        return CecTarget::Refused(format!(
            "{} çıkışının CEC bağdaştırıcısı yok",
            output.connector.name
        ));
    };
    let confidence = output.cec_confidence();
    if !confidence.actionable() {
        return CecTarget::Refused(format!(
            "{} ile verici arasındaki bağ belirsiz ({confidence:?}); CEC komutu başka bir \
             televizyona gidebilir, gönderilmiyor",
            output.connector.name
        ));
    }
    CecTarget::Adapter {
        path: adapter.device.clone(),
        expected_address: output.connector.physical_address,
        confidence,
    }
}

/// What a sending path needs to know of an adapter.
pub trait CecPort {
    fn path(&self) -> &Path;
    /// The physical and logical address the kernel holds for it now.
    fn addresses(&self) -> (u16, Option<u8>);
}

impl CecPort for mediabox_cec::Adapter {
    fn path(&self) -> &Path {
        mediabox_cec::Adapter::path(self)
    }
    fn addresses(&self) -> (u16, Option<u8>) {
        mediabox_cec::Adapter::addresses(self)
    }
}

const NO_ADDRESS: u16 = 0xFFFF;

/// The open adapter a command may be sent down, or why none may.
pub fn choose<P: CecPort>(ports: &[Arc<P>], target: &CecTarget) -> Result<Arc<P>, String> {
    let (path, expected) = match target {
        CecTarget::Refused(why) => return Err(why.clone()),
        CecTarget::Adapter {
            path,
            expected_address,
            ..
        } => (path, *expected_address),
    };
    let port = ports
        .iter()
        .find(|port| port.path() == path.as_path())
        .ok_or_else(|| format!("seçili çıkışın CEC bağdaştırıcısı {} açık değil", path.display()))?;
    let (physical, logical) = port.addresses();
    if physical == NO_ADDRESS {
        return Err(format!(
            "{}: fiziksel adres yok (f.f.f.f); televizyon bu soketi görmüyor",
            path.display()
        ));
    }
    if logical.is_none() {
        return Err(format!("{}: mantıksal adres alınamadı", path.display()));
    }
    if let Some(expected) = expected
        && expected != physical
    {
        return Err(format!(
            "{}: bağdaştırıcı {} tutuyor, seçili ekranın EDID'i {} bildiriyor",
            path.display(),
            mediabox_cec::format_physical_address(physical),
            mediabox_cec::format_physical_address(expected)
        ));
    }
    Ok(Arc::clone(port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Fake {
        path: PathBuf,
        physical: u16,
        logical: Option<u8>,
    }

    impl CecPort for Fake {
        fn path(&self) -> &Path {
            &self.path
        }
        fn addresses(&self) -> (u16, Option<u8>) {
            (self.physical, self.logical)
        }
    }

    fn port(name: &str, physical: u16, logical: Option<u8>) -> Arc<Fake> {
        Arc::new(Fake {
            path: PathBuf::from(format!("/dev/{name}")),
            physical,
            logical,
        })
    }

    fn to(name: &str, expected: Option<u16>) -> CecTarget {
        CecTarget::Adapter {
            path: PathBuf::from(format!("/dev/{name}")),
            expected_address: expected,
            confidence: Confidence::Measured,
        }
    }

    #[test]
    fn two_live_adapters_the_command_goes_to_the_selected_outputs() {
        // cec0 live, cec1 live; the selected transmitter's adapter is cec1.
        let ports = [port("cec0", 0x1000, Some(4)), port("cec1", 0x3000, Some(4))];
        let chosen = choose(&ports, &to("cec1", Some(0x3000))).unwrap();
        assert_eq!(chosen.path(), Path::new("/dev/cec1"));
    }

    #[test]
    fn an_ambiguous_topology_sends_nothing() {
        let ports = [port("cec0", 0x1000, Some(4)), port("cec1", 0x1000, Some(4))];
        let error = choose(&ports, &CecTarget::Refused("belirsiz".into())).unwrap_err();
        assert_eq!(error, "belirsiz");
    }

    #[test]
    fn a_selected_adapter_with_no_address_is_an_error_not_a_fallback() {
        // The other adapter is live; it is still not where the command goes.
        let ports = [port("cec0", 0x1000, Some(4)), port("cec1", NO_ADDRESS, None)];
        let error = choose(&ports, &to("cec1", None)).unwrap_err();
        assert!(error.contains("f.f.f.f"), "{error}");
        let ports = [port("cec0", 0x1000, Some(4)), port("cec1", 0x3000, None)];
        assert!(choose(&ports, &to("cec1", None)).unwrap_err().contains("mantıksal"));
    }

    #[test]
    fn an_adapter_holding_another_sinks_address_is_refused() {
        let ports = [port("cec1", 0x2000, Some(4))];
        let error = choose(&ports, &to("cec1", Some(0x3000))).unwrap_err();
        assert!(error.contains("2.0.0.0") && error.contains("3.0.0.0"), "{error}");
    }

    #[test]
    fn a_selected_adapter_that_is_not_open_is_refused() {
        let ports = [port("cec0", 0x1000, Some(4))];
        assert!(choose(&ports, &to("cec1", None)).unwrap_err().contains("/dev/cec1"));
    }
}
