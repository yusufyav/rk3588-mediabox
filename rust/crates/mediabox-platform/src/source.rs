//! What the source can send, as a named, versioned profile, and the check
//! that the running system is the one the profile was written for.
//!
//! The HDMI rules in `video` need the transmitter's own limits: the fastest
//! character rate it drives, the deepest colour, the formats, and the vendor
//! properties those are asked for through. They were one anonymous constant
//! that nothing checked. A profile keeps them in three parts, because they are
//! true for three different reasons:
//!
//! * **product scope** -- which system the profile is for: the SoC, the
//!   kernel series and the transmitter's device-tree compatible;
//! * **vendor ABI assumptions** -- how that kernel's driver is spoken to: the
//!   `color_format` and `color_depth` enums and their values, and the HDR
//!   metadata property. Nothing standard defines these; the vendor 6.1 tree
//!   does (drivers/gpu/drm/rockchip/dw_hdmi-rockchip.c);
//! * **link limits** -- what the driver lets through (`SourceCaps`).
//!
//! Each part has a runtime-observable counterpart ([`SourceSignature`]). A
//! profile is used only when the part that can be observed matches. When it
//! does not, the answer is [`CONSERVATIVE`]: RGB, eight bits, SDR, whatever
//! mode the kernel listed. There is no fallback to a profile shaped around one
//! particular display.

use serde::{Deserialize, Serialize};

use crate::video::SourceCaps;
use mediabox_core::ColorFormat;

/// The product scope: which system a profile describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductScope {
    /// A string in the machine's root `compatible`.
    pub soc_compatible: &'static str,
    /// `uname -r` begins with this.
    pub kernel_series: &'static str,
    /// The HDMI transmitter's `compatible`.
    pub transmitter_compatible: &'static str,
    /// The DRM driver bound to the display device.
    pub kms_driver: &'static str,
}

/// The vendor ABI a profile assumes: connector properties by name, and the
/// enum values the product writes to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorAbi {
    /// `color_format`: each format the product sends, by the enum name the
    /// driver gives it and the value it has.
    pub color_format: &'static [(ColorFormat, &'static str, u64)],
    /// `color_depth`: bits per component, the enum name, the value.
    pub color_depth: &'static [(u8, &'static str, u64)],
    /// The standard HDR infoframe property must be there for HDR.
    pub hdr_output_metadata: bool,
    /// The standard `Colorspace` property, with the BT.2020 entries HDR uses.
    pub colorspace: &'static [&'static str],
}

/// A named, versioned description of a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceProfile {
    pub id: &'static str,
    pub version: u32,
    pub scope: Option<ProductScope>,
    pub abi: Option<VendorAbi>,
    pub caps: SourceCaps,
    /// Whether HDR may be signalled at all under this profile. The HDR
    /// eligibility rules themselves are elsewhere; this is only the source's
    /// half of them.
    pub hdr: bool,
}

impl SourceProfile {
    /// `rk3588-vendor61-dw-hdmi-qp@1`.
    pub fn name(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }

    /// The value this profile's `color_format` gives `format`. `None` under a
    /// profile with no vendor ABI -- the property is then left to the driver,
    /// because a value is only a format on the driver it was read from.
    pub fn color_format_value(&self, format: ColorFormat) -> Option<u64> {
        self.abi?
            .color_format
            .iter()
            .find(|(have, _, _)| *have == format)
            .map(|(_, _, value)| *value)
    }

    /// The value this profile's `color_depth` gives `bits` per component.
    pub fn color_depth_value(&self, bits: u8) -> Option<u64> {
        self.abi?
            .color_depth
            .iter()
            .find(|(have, _, _)| *have == bits)
            .map(|(_, _, value)| *value)
    }
}

/// Rockchip RK3588 under the vendor 6.1 kernel, HDMI through the Synopsys
/// DesignWare HDMI QP transmitter -- the stack this product ships.
///
/// Link limits as the vendor driver enforces them: `dw_hdmi_rockchip_mode_valid`
/// stops TMDS at 600 MHz; `color_depth` offers 24 and 30 bit, so ten bits per
/// component at most; `color_format` offers RGB, 4:4:4, 4:2:2 and 4:2:0.
pub const RK3588_VENDOR_61: SourceProfile = SourceProfile {
    id: "rk3588-vendor61-dw-hdmi-qp",
    version: 1,
    scope: Some(ProductScope {
        soc_compatible: "rockchip,rk3588",
        kernel_series: "6.1.",
        transmitter_compatible: "rockchip,rk3588-dw-hdmi",
        kms_driver: "rockchip-drm",
    }),
    abi: Some(VendorAbi {
        color_format: &[
            (ColorFormat::Rgb, "rgb", 0),
            (ColorFormat::Ycbcr444, "ycbcr444", 1),
            (ColorFormat::Ycbcr422, "ycbcr422", 2),
            (ColorFormat::Ycbcr420, "ycbcr420", 3),
        ],
        color_depth: &[(8, "24bit", 8), (10, "30bit", 10)],
        hdr_output_metadata: true,
        colorspace: &["BT2020_RGB", "BT2020_YCC"],
    }),
    caps: SourceCaps {
        max_tmds_khz: 600_000,
        max_bpc: 10,
        formats: &[
            ColorFormat::Rgb,
            ColorFormat::Ycbcr444,
            ColorFormat::Ycbcr422,
            ColorFormat::Ycbcr420,
        ],
    },
    hdr: true,
};

/// What any source can be trusted with when the system is not the one a
/// profile was written for: RGB at eight bits, SDR, on whatever modes the
/// kernel lists. The character-rate ceiling is the kernel's own mode
/// filtering, so nothing is refused here for the source's sake.
pub const CONSERVATIVE: SourceProfile = SourceProfile {
    id: "conservative-rgb8-sdr",
    version: 1,
    scope: None,
    abi: None,
    caps: SourceCaps {
        max_tmds_khz: u32::MAX,
        max_bpc: 8,
        formats: &[ColorFormat::Rgb],
    },
    hdr: false,
};

/// The profiles this build knows, most specific first.
pub const PROFILES: &[&SourceProfile] = &[&RK3588_VENDOR_61];

/// What the running system says about itself, gathered without opening the
/// display device: sysfs, the device tree and `uname`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaticSignature {
    pub root_compatible: Vec<String>,
    pub kernel_release: Option<String>,
    pub kms_driver: Option<String>,
    /// The `compatible` of the transmitter behind the selected output.
    pub transmitter_compatible: Vec<String>,
}

/// One enum property of the connector: every name, with its value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumProperty {
    pub present: bool,
    pub values: Vec<(String, u64)>,
}

impl EnumProperty {
    fn has(&self, name: &str, value: u64) -> bool {
        self.values
            .iter()
            .any(|(have, raw)| have == name && *raw == value)
    }
}

/// The connector's properties, read by whoever holds a descriptor on the
/// display device (the interface, which is DRM master; or a read-only query
/// that found a master in place). `None` in [`SourceSignature::properties`]
/// means nobody could read them without taking the display, and nobody tried.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertySignature {
    pub color_format: EnumProperty,
    pub color_depth: EnumProperty,
    pub hdr_output_metadata: bool,
    pub colorspace: EnumProperty,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSignature {
    pub static_part: StaticSignature,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<PropertySignature>,
}

/// How a profile came to be chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "match", rename_all = "snake_case")]
pub enum ProfileMatch {
    /// The product scope and the vendor ABI both matched.
    Matched,
    /// The product scope matched; the properties could not be read without
    /// taking the display, so the ABI is assumed from the scope until they
    /// are. Not a mismatch: nothing observed disagrees.
    PropertiesUnverified,
    /// Something observed disagrees with every profile: the conservative one
    /// is in use, and here is why.
    Mismatch { reasons: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub profile: &'static SourceProfile,
    pub matched: ProfileMatch,
}

impl Resolved {
    /// `rk3588-vendor61-dw-hdmi-qp@1 (matched)`: for logs and the offer.
    pub fn describe(&self) -> String {
        let how = match &self.matched {
            ProfileMatch::Matched => "matched".to_string(),
            ProfileMatch::PropertiesUnverified => "properties unverified".to_string(),
            ProfileMatch::Mismatch { reasons } => format!("mismatch: {}", reasons.join("; ")),
        };
        format!("{} ({how})", self.profile.name())
    }
}

/// Choose the profile the running system is, or [`CONSERVATIVE`].
pub fn resolve(signature: &SourceSignature) -> Resolved {
    let mut reasons = Vec::new();
    for profile in PROFILES {
        let mut wrong = scope_mismatch(profile, &signature.static_part);
        if wrong.is_empty() {
            match &signature.properties {
                None => {
                    return Resolved {
                        profile,
                        matched: ProfileMatch::PropertiesUnverified,
                    };
                }
                Some(properties) => {
                    wrong = abi_mismatch(profile, properties);
                    if wrong.is_empty() {
                        return Resolved {
                            profile,
                            matched: ProfileMatch::Matched,
                        };
                    }
                }
            }
        }
        reasons.extend(wrong.into_iter().map(|reason| format!("{}: {reason}", profile.name())));
    }
    Resolved {
        profile: &CONSERVATIVE,
        matched: ProfileMatch::Mismatch { reasons },
    }
}

fn scope_mismatch(profile: &SourceProfile, seen: &StaticSignature) -> Vec<String> {
    let Some(scope) = profile.scope else {
        return vec!["no product scope".into()];
    };
    let mut wrong = Vec::new();
    if !seen.root_compatible.iter().any(|c| c == scope.soc_compatible) {
        wrong.push(format!("SoC is not {}", scope.soc_compatible));
    }
    if !seen
        .kernel_release
        .as_deref()
        .is_some_and(|release| release.starts_with(scope.kernel_series))
    {
        wrong.push(format!(
            "kernel {} is not {}x",
            seen.kernel_release.as_deref().unwrap_or("?"),
            scope.kernel_series
        ));
    }
    if seen.kms_driver.as_deref() != Some(scope.kms_driver) {
        wrong.push(format!(
            "display driver {} is not {}",
            seen.kms_driver.as_deref().unwrap_or("?"),
            scope.kms_driver
        ));
    }
    if !seen
        .transmitter_compatible
        .iter()
        .any(|c| c == scope.transmitter_compatible)
    {
        wrong.push(format!(
            "transmitter is not {}",
            scope.transmitter_compatible
        ));
    }
    wrong
}

fn abi_mismatch(profile: &SourceProfile, seen: &PropertySignature) -> Vec<String> {
    let Some(abi) = profile.abi else {
        return vec!["no vendor ABI".into()];
    };
    let mut wrong = Vec::new();
    if !seen.color_format.present {
        wrong.push("no color_format property".into());
    }
    for (_, name, value) in abi.color_format {
        if seen.color_format.present && !seen.color_format.has(name, *value) {
            wrong.push(format!("color_format has no {name}={value}"));
        }
    }
    if !seen.color_depth.present {
        wrong.push("no color_depth property".into());
    }
    for (_, name, value) in abi.color_depth {
        if seen.color_depth.present && !seen.color_depth.has(name, *value) {
            wrong.push(format!("color_depth has no {name}={value}"));
        }
    }
    if abi.hdr_output_metadata && !seen.hdr_output_metadata {
        wrong.push("no HDR_OUTPUT_METADATA property".into());
    }
    for name in abi.colorspace {
        if !seen
            .colorspace
            .values
            .iter()
            .any(|(have, _)| have == name)
        {
            wrong.push(format!("Colorspace has no {name}"));
        }
    }
    wrong
}

/// The static half of the signature, read from `roots`: the machine's
/// `compatible`, the running kernel's release, the display device's driver
/// and the selected output's transmitter.
pub fn static_signature(
    roots: &crate::Roots,
    platform: &crate::Platform,
) -> StaticSignature {
    let strings = |path: std::path::PathBuf| -> Vec<String> {
        std::fs::read(path)
            .map(|bytes| {
                bytes
                    .split(|byte| *byte == 0)
                    .filter(|part| !part.is_empty())
                    .map(|part| String::from_utf8_lossy(part).into_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    let transmitter = platform
        .selected_output()
        .and_then(|output| output.controller.as_ref())
        .and_then(|device| {
            platform
                .controllers
                .iter()
                .find(|controller| &controller.device == device)
        });
    StaticSignature {
        root_compatible: strings(roots.sys("firmware/devicetree/base/compatible")),
        kernel_release: crate::roots::read_trimmed(roots.proc("sys/kernel/osrelease")),
        kms_driver: platform.kms.as_ref().and_then(|node| node.driver.clone()),
        transmitter_compatible: transmitter
            .map(|controller| strings(controller.sysfs.join("of_node/compatible")))
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plus() -> StaticSignature {
        StaticSignature {
            root_compatible: vec!["rockchip,rk3588-orangepi-5-plus".into(), "rockchip,rk3588".into()],
            kernel_release: Some("6.1.115-vendor-rk35xx".into()),
            kms_driver: Some("rockchip-drm".into()),
            transmitter_compatible: vec!["rockchip,rk3588-dw-hdmi".into()],
        }
    }

    /// As the vendor driver enumerates them on the Plus (modetest, 2026-09-24).
    fn vendor_properties() -> PropertySignature {
        let e = |pairs: &[(&str, u64)]| EnumProperty {
            present: true,
            values: pairs.iter().map(|(n, v)| (n.to_string(), *v)).collect(),
        };
        PropertySignature {
            color_format: e(&[
                ("rgb", 0),
                ("ycbcr444", 1),
                ("ycbcr422", 2),
                ("ycbcr420", 3),
                ("ycbcr_high_subsampling", 4),
                ("ycbcr_low_subsampling", 5),
                ("invalid_output", 6),
            ]),
            color_depth: e(&[("Automatic", 0), ("24bit", 8), ("30bit", 10)]),
            hdr_output_metadata: true,
            colorspace: e(&[("Default", 0), ("BT2020_RGB", 9), ("BT2020_YCC", 10)]),
        }
    }

    #[test]
    fn the_shipping_stack_is_its_own_profile() {
        let resolved = resolve(&SourceSignature {
            static_part: plus(),
            properties: Some(vendor_properties()),
        });
        assert_eq!(resolved.profile, &RK3588_VENDOR_61);
        assert_eq!(resolved.matched, ProfileMatch::Matched);
        assert_eq!(resolved.profile.name(), "rk3588-vendor61-dw-hdmi-qp@1");
        // The limits the display screen has always used.
        assert_eq!(resolved.profile.caps.max_tmds_khz, 600_000);
        assert_eq!(resolved.profile.caps.max_bpc, 10);
        assert_eq!(resolved.profile.caps.formats.len(), 4);
    }

    #[test]
    fn unread_properties_are_not_a_mismatch() {
        let resolved = resolve(&SourceSignature {
            static_part: plus(),
            properties: None,
        });
        assert_eq!(resolved.profile, &RK3588_VENDOR_61);
        assert_eq!(resolved.matched, ProfileMatch::PropertiesUnverified);
    }

    #[test]
    fn another_kernel_is_the_conservative_profile() {
        let mut other = plus();
        other.kernel_release = Some("6.12.4-mainline".into());
        let resolved = resolve(&SourceSignature {
            static_part: other,
            properties: Some(vendor_properties()),
        });
        assert_eq!(resolved.profile, &CONSERVATIVE);
        assert_eq!(resolved.profile.caps.formats, &[ColorFormat::Rgb]);
        assert_eq!(resolved.profile.caps.max_bpc, 8);
        assert!(!resolved.profile.hdr);
        assert!(matches!(&resolved.matched, ProfileMatch::Mismatch { reasons } if reasons.iter().any(|r| r.contains("kernel"))));
    }

    #[test]
    fn a_driver_whose_enums_moved_is_the_conservative_profile() {
        // Mainline's `Broadcast RGB`-era connector: no vendor color_format.
        let mut properties = vendor_properties();
        properties.color_format = EnumProperty::default();
        let resolved = resolve(&SourceSignature {
            static_part: plus(),
            properties: Some(properties),
        });
        assert_eq!(resolved.profile, &CONSERVATIVE);

        // The same names at other values is not the same ABI either: the
        // product writes values, not names.
        let mut shifted = vendor_properties();
        shifted.color_format.values = vec![
            ("rgb".into(), 0),
            ("ycbcr422".into(), 1),
            ("ycbcr444".into(), 2),
            ("ycbcr420".into(), 3),
        ];
        let resolved = resolve(&SourceSignature {
            static_part: plus(),
            properties: Some(shifted),
        });
        assert_eq!(resolved.profile, &CONSERVATIVE);
        assert!(resolved.describe().contains("color_format has no ycbcr444=1"));
    }

    #[test]
    fn another_transmitter_is_the_conservative_profile() {
        let mut dp = plus();
        dp.transmitter_compatible = vec!["rockchip,rk3588-dp".into()];
        assert_eq!(
            resolve(&SourceSignature { static_part: dp, properties: None }).profile,
            &CONSERVATIVE
        );
    }

    #[test]
    fn the_conservative_profile_sends_rgb_eight_bit_sdr_only() {
        use crate::video::{SinkVideo, Timing};
        use mediabox_core::ColorMode;
        let sink = SinkVideo {
            max_character_rate_khz: 600_000,
            rate_is_declared: true,
            advertised: Vec::new(),
            st2084: true,
            hlg: true,
            is_hdmi: true,
            ycbcr444: true,
            ycbcr422: true,
            rgb_deep: vec![10, 12],
            ycbcr444_deep: vec![10, 12],
            ycbcr420_deep: vec![10, 12],
            y420_only: Vec::new(),
            y420_also: vec![97],
        };
        let uhd60 = Timing::new(3840, 2160, 594_000, 4400, 2250, false, false);
        assert_eq!(
            sink.modes_for_source(&uhd60, &CONSERVATIVE.caps),
            vec![ColorMode::new(ColorFormat::Rgb, 8)]
        );
    }
}
