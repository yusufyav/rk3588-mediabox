fn main() {
    // slint-build refuses the interface's warnings when this is set
    // (scripts/cargo-no-warnings.sh) but does not ask cargo to rerun when it
    // changes; without this a cached build would never be checked again.
    println!("cargo:rerun-if-env-changed=SLINT_COMPILER_DENY_WARNINGS");
    slint_build::compile("ui/app.slint").expect("the interface did not compile");
}
