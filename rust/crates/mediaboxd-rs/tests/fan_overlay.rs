//! The fan curve overlay, read and applied by the reference device-tree tools.
//!
//! A test binary of its own, so the library's unit tests spawn no processes.
//! `transition::tests` checks `flock` state, and a child between fork and exec
//! holds a copy of every descriptor; that test is already intermittent without
//! this file (measured: 5 of 20 runs of the library tests failed at 06180fb),
//! and nothing here should add to it.
//!
//! Skipped, with a line saying so, where the tools are not installed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use mediabox_core::{FanCurve, FanPoint, FanProfile};
use mediaboxd_rs::fan::curve_overlay;

fn tool(name: &str) -> Option<PathBuf> {
    ["/usr/bin", "/usr/local/bin", "/bin"]
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|path| path.is_file())
}

fn run(command: &mut Command) -> String {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// Applies `curve` to a tree shaped like the board's and returns the two
/// properties and the carrier as `fdtget` prints them.
fn apply(curve: &FanCurve) -> Option<(String, String, String)> {
    let (Some(dtc), Some(fdtoverlay), Some(fdtget)) =
        (tool("dtc"), tool("fdtoverlay"), tool("fdtget"))
    else {
        eprintln!("dtc/fdtoverlay/fdtget not installed; skipped");
        return None;
    };
    let dir = tempfile::tempdir().unwrap();
    let base_dts = dir.path().join("base.dts");
    fs::write(
        &base_dts,
        "/dts-v1/;\n/ {\n  pwm3: pwm@febd0030 { #pwm-cells = <3>; };\n  \
         fan: pwm-fan {\n    compatible = \"pwm-fan\";\n    \
         pwms = <&pwm3 0 20000000 0>;\n    cooling-levels = <0 50 100 150 200 255>;\n    \
         rockchip,temp-trips = <50000 1 55000 2 60000 3 65000 4 70000 5>;\n  };\n};\n",
    )
    .unwrap();
    let base = dir.path().join("base.dtb");
    let overlay = dir.path().join("curve.dtbo");
    let merged = dir.path().join("merged.dtb");
    run(Command::new(&dtc)
        .args(["-@", "-I", "dts", "-O", "dtb", "-o"])
        .arg(&base)
        .arg(&base_dts));
    fs::write(&overlay, curve_overlay("/pwm-fan", curve)).unwrap();
    let source = run(Command::new(&dtc)
        .args(["-I", "dtb", "-O", "dts"])
        .arg(&overlay));
    assert!(source.contains("target-path = \"/pwm-fan\""), "{source}");
    run(Command::new(&fdtoverlay)
        .arg("-i")
        .arg(&base)
        .arg("-o")
        .arg(&merged)
        .arg(&overlay));
    let get = |property: &str| {
        run(Command::new(&fdtget)
            .arg(&merged)
            .arg("/pwm-fan")
            .arg(property))
        .trim()
        .to_string()
    };
    Some((
        get("cooling-levels"),
        get("rockchip,temp-trips"),
        get("pwms"),
    ))
}

fn custom(points: &[(u32, u32)]) -> FanCurve {
    let curve = FanCurve {
        profile: FanProfile::Custom,
        points: points.iter().map(|&(t, p)| FanPoint::new(t, p)).collect(),
    };
    curve.validate().unwrap();
    curve
}

#[test]
fn a_five_point_curve_replaces_the_vendor_one() {
    let curve = custom(&[(45, 60), (52, 90), (58, 140), (64, 200), (72, 255)]);
    let Some((levels, trips, pwms)) = apply(&curve) else {
        return;
    };
    assert_eq!(levels, expected_levels(&curve));
    assert_eq!(trips, expected_trips(&curve));
    assert!(levels.starts_with("0 60 64 69 "));
    assert!(trips.starts_with("45000 1 46000 2 "));
    // The carrier is not the curve's business.
    assert_eq!(pwms.split_whitespace().nth(2), Some("20000000"));
}

/// More states than the vendor tree has, starting at 0 °C: the overlay
/// replaces both arrays whole, so the count is the curve's -- one per degree
/// from where the fan first runs.
#[test]
fn a_longer_curve_from_zero_degrees_is_applied_whole() {
    let curve = custom(&[
        (0, 0),
        (30, 0),
        (40, 50),
        (48, 80),
        (55, 120),
        (63, 180),
        (72, 255),
    ]);
    let Some((levels, trips, _)) = apply(&curve) else {
        return;
    };
    assert_eq!(levels, expected_levels(&curve));
    assert_eq!(trips, expected_trips(&curve));
    assert_eq!(levels.split_whitespace().count(), 34);
}

#[test]
fn a_two_point_curve_is_applied_whole() {
    let curve = custom(&[(35, 50), (70, 255)]);
    let Some((levels, trips, _)) = apply(&curve) else {
        return;
    };
    assert_eq!(levels, expected_levels(&curve));
    assert_eq!(trips, expected_trips(&curve));
    assert!(trips.ends_with("70000 36"));
}

/// What fdtget prints for the arrays the curve gives the kernel.
fn expected_levels(curve: &FanCurve) -> String {
    let levels: Vec<String> = curve.cooling_levels().iter().map(u32::to_string).collect();
    levels.join(" ")
}

fn expected_trips(curve: &FanCurve) -> String {
    let trips: Vec<String> = curve
        .temp_trips()
        .into_iter()
        .flat_map(|(t, s)| [t.to_string(), s.to_string()])
        .collect();
    trips.join(" ")
}
