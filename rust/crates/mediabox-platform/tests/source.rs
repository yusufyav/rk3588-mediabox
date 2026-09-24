//! The source profile, chosen from what a captured board says about itself.

mod common;

use std::fs;

use common::one_hdmi;
use mediabox_platform::source::{
    CONSERVATIVE, ProfileMatch, RK3588_VENDOR_61, SourceSignature, resolve, static_signature,
};
use mediabox_platform::{Overrides, Platform, Roots};

fn describe(board: &common::Board, soc: &str, kernel: &str, transmitter: &str) -> SourceSignature {
    let root = board.root();
    fs::write(
        root.join("sys/firmware/devicetree/base/compatible"),
        format!("vendor,board\0{soc}\0"),
    )
    .unwrap();
    fs::create_dir_all(root.join("proc/sys/kernel")).unwrap();
    fs::write(root.join("proc/sys/kernel/osrelease"), format!("{kernel}\n")).unwrap();
    fs::write(
        root.join("sys/firmware/devicetree/base/hdmi@fdea0000/compatible"),
        format!("{transmitter}\0"),
    )
    .unwrap();
    let roots = Roots::under(root);
    let platform = Platform::inspect(&roots, &Overrides::default());
    SourceSignature {
        static_part: static_signature(&roots, &platform),
        properties: None,
    }
}

#[test]
fn the_shipping_board_is_read_as_the_shipping_profile() {
    let board = one_hdmi();
    let signature = describe(&board, "rockchip,rk3588", "6.1.115-vendor-rk35xx", "rockchip,rk3588-dw-hdmi");
    assert_eq!(signature.static_part.kms_driver.as_deref(), Some("rockchip-drm"));
    assert_eq!(signature.static_part.transmitter_compatible, vec!["rockchip,rk3588-dw-hdmi"]);
    let resolved = resolve(&signature);
    assert_eq!(resolved.profile, &RK3588_VENDOR_61);
    // No descriptor on the display device here, so the ABI is unverified,
    // which is not a mismatch.
    assert_eq!(resolved.matched, ProfileMatch::PropertiesUnverified);
}

#[test]
fn a_board_that_is_not_the_one_the_profile_is_for_is_conservative() {
    for (soc, kernel, transmitter) in [
        ("rockchip,rk3576", "6.1.115-vendor-rk35xx", "rockchip,rk3588-dw-hdmi"),
        ("rockchip,rk3588", "6.12.1", "rockchip,rk3588-dw-hdmi"),
        ("rockchip,rk3588", "6.1.115-vendor-rk35xx", "rockchip,rk3588-dw-hdmi-qp"),
    ] {
        let board = one_hdmi();
        let resolved = resolve(&describe(&board, soc, kernel, transmitter));
        assert_eq!(resolved.profile, &CONSERVATIVE, "{soc} {kernel} {transmitter}");
        assert!(matches!(resolved.matched, ProfileMatch::Mismatch { .. }));
    }
}
