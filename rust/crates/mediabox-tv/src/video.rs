//! The video plane.
//!
//! MediaBox's own player draws here rather than into the interface's own
//! surface. The display controller on this board can drive two windows of one
//! video port at once, and it will scale and convert one of them for free:
//!
//!   Video Port0 PLANE_MASK = Cluster0 | Esmart0
//!   plane 57  Cluster0        type Primary   zpos 0    XR24 AR24 …
//!   plane 73  Esmart0-win0    zpos 11        FEATURE scale=0x1
//!             NV12 NV21 NV16 NV61 NV24 NV42 NV15 NV20 NV30 …
//!             modifiers NV12: LINEAR, NV15: LINEAR
//!
//! Measured on the appliance with modetest. That is the whole argument for this
//! module: the interface keeps Cluster0 and keeps DRM master, a decoded frame
//! goes straight onto Esmart0 as the buffer the decoder produced, and the
//! scaler in the display controller puts it where the interface says. Nothing
//! is copied, nothing is converted on the CPU, and the interface does not have
//! to hand the television to another process to show a film.
//!
//! What this module does *not* do is decode. Frames arrive as dma-buf file
//! descriptors from whoever produced them; this imports them and puts them on
//! the plane.

use std::os::fd::{AsFd, BorrowedFd};

use drm::buffer::{DrmFourcc, DrmModifier, Handle as BufferHandle, PlanarBuffer};
use drm::control::{self, Device as ControlDevice};

/// One decoded frame, as the display controller needs to see it.
pub struct Frame {
    pub fourcc: DrmFourcc,
    pub width: u32,
    pub height: u32,
    /// Bytes per row of each plane. NV12 and NV15 use two.
    pub pitches: [u32; 4],
    pub offsets: [u32; 4],
    pub planes: usize,
    /// How the bytes are arranged. The decoder says so rather than this module
    /// assuming: MPP hands back LINEAR on this board, but a tiled AFBC frame
    /// from something else must not be imported as if it were not.
    pub modifier: u64,
    /// What the numbers in the buffer mean. The plane converts YCbCr to RGB in
    /// hardware and has to be told which matrix and which range to use; it
    /// defaults to BT.601 limited, which is right for a DVD and wrong for
    /// everything this appliance is for.
    pub colour: Colour,
}

/// What staging a frame needs to know about it once its buffers are imported.
#[derive(Clone, Copy, Debug)]
pub struct FrameInfo {
    pub fourcc: DrmFourcc,
    pub width: u32,
    pub height: u32,
    pub colour: Colour,
}

/// The two plane properties that decide how a frame is converted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Colour {
    /// 0 = BT.601, 1 = BT.709, 2 = BT.2020. The plane's own enum order.
    pub encoding: u64,
    /// 0 = limited, 1 = full.
    pub range: u64,
    /// The vendor driver's `EOTF`: 0 traditional gamma (SDR), 2 SMPTE ST 2084,
    /// 3 BT.2100 HLG.
    ///
    /// This one is not a nicety. `EOTF` belongs to the plane and outlives the
    /// process that set it: Kodi leaves it on ST 2084 after an HDR film, and
    /// a player that never writes it inherits that for the next SDR film.
    /// Measured on the Plus after a Kodi handover -- `format: NV12  color:
    /// HDR10[2]` -- and on the television, magenta.
    pub eotf: u64,
}

/// What the film says about itself: the CTA-861 Dynamic Range and Mastering
/// InfoFrame, as the player read it out of the stream.
///
/// Kept apart from `Colour` because it is a different question answered by a
/// different property. `Colour` is how the display controller must *read* the
/// plane; this is what the television is *told* it is being sent, and only the
/// connector can be told.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct HdrStatic {
    /// 0 traditional gamma (SDR), 2 SMPTE ST 2084, 3 BT.2100 HLG.
    pub eotf: u8,
    /// Mastering display primaries and white point, in CTA-861's 0.00002
    /// steps: red x/y, green x/y, blue x/y, white x/y.
    pub primaries: [u16; 8],
    /// Peak in cd/m², floor in 0.0001 cd/m².
    pub max_luminance: u16,
    pub min_luminance: u16,
    pub max_cll: u16,
    pub max_fall: u16,
}

impl HdrStatic {
    /// Whether this asks for HDR at all.
    pub fn is_hdr(&self) -> bool {
        self.eotf == 2 || self.eotf == 3
    }

    /// The kernel's `struct hdr_output_metadata`, as the connector's blob
    /// wants it: a type word, then the infoframe.
    pub fn blob(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        // metadata_type = 0 (HDMI static metadata type 1)
        out[4] = self.eotf;
        out[5] = 0; // metadata_type inside the infoframe: static metadata 1
        for (i, value) in self.primaries.iter().enumerate() {
            out[6 + i * 2..8 + i * 2].copy_from_slice(&value.to_le_bytes());
        }
        out[22..24].copy_from_slice(&self.max_luminance.to_le_bytes());
        out[24..26].copy_from_slice(&self.min_luminance.to_le_bytes());
        out[26..28].copy_from_slice(&self.max_cll.to_le_bytes());
        out[28..30].copy_from_slice(&self.max_fall.to_le_bytes());
        out
    }
}

/// A frame that has been imported and given a framebuffer, kept until the
/// display controller is finished with it.
pub struct Imported<D: ControlDevice> {
    device: D,
    framebuffer: control::framebuffer::Handle,
    handles: Vec<BufferHandle>,
}

impl<D: ControlDevice> Drop for Imported<D> {
    fn drop(&mut self) {
        let _ = self.device.destroy_framebuffer(self.framebuffer);
        for handle in &self.handles {
            let _ = self.device.close_buffer(*handle);
        }
    }
}

struct Planar {
    fourcc: DrmFourcc,
    size: (u32, u32),
    pitches: [u32; 4],
    handles: [Option<BufferHandle>; 4],
    offsets: [u32; 4],
    modifier: DrmModifier,
}

impl drm::buffer::Buffer for Planar {
    fn size(&self) -> (u32, u32) {
        self.size
    }
    fn format(&self) -> DrmFourcc {
        self.fourcc
    }
    fn pitch(&self) -> u32 {
        self.pitches[0]
    }
    fn handle(&self) -> BufferHandle {
        self.handles[0].expect("a frame with no first plane")
    }
}

impl PlanarBuffer for Planar {
    fn size(&self) -> (u32, u32) {
        self.size
    }
    fn format(&self) -> DrmFourcc {
        self.fourcc
    }
    fn modifier(&self) -> Option<DrmModifier> {
        // Whatever the decoder said. The Esmart plane advertises NV12 and NV15
        // as LINEAR and that is what MPP produces here, but the frame is the
        // authority on its own layout — saying so explicitly rather than
        // passing Invalid, because ADDFB2 without the modifier flag is a
        // different call.
        Some(self.modifier)
    }
    fn pitches(&self) -> [u32; 4] {
        self.pitches
    }
    fn handles(&self) -> [Option<BufferHandle>; 4] {
        self.handles
    }
    fn offsets(&self) -> [u32; 4] {
        self.offsets
    }
}

/// Put the interface's own window above the film's.
///
/// The display controller composites the video port's windows by `zpos`, and
/// on this board both windows carry that property as a range of 0 to 11: the
/// primary starts at 0 and Esmart0 at 11, so a film covers the interface
/// completely. That is right for the picture and wrong for everything else —
/// there is then nowhere to draw what is playing, how far in it is, or which
/// button the remote is on, and a film plays with no way to see that it can be
/// controlled at all.
///
/// Swapping them is only half of it: the interface's surface has to carry alpha
/// for the film to show through the parts it does not draw, which is why the
/// scanout buffers are ARGB8888 rather than XRGB8888. With both, the interface
/// is an overlay on the film exactly the way it is an overlay on its own
/// wallpaper.
///
/// Returns the plane it raised, for the journal.
pub fn raise_interface<D: ControlDevice>(
    device: &D,
    crtc: control::crtc::Handle,
) -> Option<String> {
    stack_interface(device, crtc, true)
}

/// Put it back where it was found.
///
/// The stacking order belongs to the connector's own state and outlives the
/// process that set it — the same property that left an HDR colourspace on this
/// panel after a film. Kodi assigns this port's windows for itself and must not
/// inherit a primary somebody else raised.
pub fn lower_interface<D: ControlDevice>(device: &D, crtc: control::crtc::Handle) {
    stack_interface(device, crtc, false);
}

fn stack_interface<D: ControlDevice>(
    device: &D,
    crtc: control::crtc::Handle,
    above: bool,
) -> Option<String> {
    let planes = device.plane_handles().ok()?;
    let resources = device.resource_handles().ok()?;
    for handle in planes {
        let Ok(info) = device.get_plane(handle) else {
            continue;
        };
        if !resources
            .filter_crtcs(info.possible_crtcs())
            .contains(&crtc)
        {
            continue;
        }
        // The primary is the one the interface's own page flips land on, and
        // unlike `NAME` its type is an ordinary enum this crate keeps.
        if !is_primary(device, handle) {
            continue;
        }
        let (property, top) = zpos(device, handle)?;
        let depth = if above { top } else { 0 };
        if device.set_property(handle, property, depth).is_err() {
            return None;
        }
        return Some(format!("{handle:?}@{depth}"));
    }
    None
}

fn is_primary<D: ControlDevice>(device: &D, plane: control::plane::Handle) -> bool {
    let Ok(properties) = device.get_properties(plane) else {
        return false;
    };
    let (handles, values) = properties.as_props_and_values();
    for (handle, value) in handles.iter().zip(values.iter()) {
        let Ok(info) = device.get_property(*handle) else {
            continue;
        };
        if info.name().to_str() == Ok("type") {
            // DRM_PLANE_TYPE_PRIMARY.
            return *value == 1;
        }
    }
    false
}

/// A plane's `zpos` property and the top of its range.
fn zpos<D: ControlDevice>(
    device: &D,
    plane: control::plane::Handle,
) -> Option<(control::property::Handle, u64)> {
    let properties = device.get_properties(plane).ok()?;
    for handle in properties.as_props_and_values().0.iter().copied() {
        let Ok(info) = device.get_property(handle) else {
            continue;
        };
        if info.name().to_str() != Ok("zpos") {
            continue;
        }
        let top = match info.value_type() {
            control::property::ValueType::UnsignedRange(_, high) => high,
            control::property::ValueType::SignedRange(_, high) => high.max(0) as u64,
            _ => continue,
        };
        return Some((handle, top));
    }
    None
}

/// The handles an atomic update of a plane writes: which framebuffer, on
/// which CRTC, from where in the frame to where on the panel.
#[derive(Clone, Copy)]
struct PlaneProps {
    fb: control::property::Handle,
    crtc: control::property::Handle,
    src: [control::property::Handle; 4],
    dst: [control::property::Handle; 4],
}

fn plane_props<D: ControlDevice>(device: &D, plane: control::plane::Handle) -> Option<PlaneProps> {
    let properties = device.get_properties(plane).ok()?;
    let mut named = std::collections::HashMap::new();
    for handle in properties.as_props_and_values().0.iter().copied() {
        if let Ok(info) = device.get_property(handle)
            && let Ok(name) = info.name().to_str()
        {
            named.insert(name.to_string(), handle);
        }
    }
    let get = |name: &str| named.get(name).copied();
    Some(PlaneProps {
        fb: get("FB_ID")?,
        crtc: get("CRTC_ID")?,
        src: [get("SRC_X")?, get("SRC_Y")?, get("SRC_W")?, get("SRC_H")?],
        dst: [get("CRTC_X")?, get("CRTC_Y")?, get("CRTC_W")?, get("CRTC_H")?],
    })
}

/// `COLOR_ENCODING`, `COLOR_RANGE` and `EOTF` on one plane, by name.
///
/// By name because these are the DRM properties whose identifiers differ
/// between drivers, and unlike `NAME` and `PLANE_MASK` they are ordinary enums
/// and ranges whose names this crate keeps. `EOTF` is the vendor driver's own,
/// a range of 0..=18 on this hardware.
fn colour_properties<D: ControlDevice>(
    device: &D,
    plane: control::plane::Handle,
) -> (
    Option<control::property::Handle>,
    Option<control::property::Handle>,
    Option<control::property::Handle>,
) {
    let Ok(properties) = device.get_properties(plane) else {
        return (None, None, None);
    };
    let mut encoding = None;
    let mut range = None;
    let mut eotf = None;
    for handle in properties.as_props_and_values().0.iter().copied() {
        let Ok(info) = device.get_property(handle) else {
            continue;
        };
        match info.name().to_str() {
            Ok("COLOR_ENCODING") => encoding = Some(handle),
            Ok("COLOR_RANGE") => range = Some(handle),
            Ok("EOTF") => eotf = Some(handle),
            _ => {}
        }
    }
    (encoding, range, eotf)
}

/// Where on the panel a frame goes, in the panel's own pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// The plane, once it has been found.
pub struct VideoPlane {
    plane: control::plane::Handle,
    /// What the plane said it could show, so a format it cannot is refused here
    /// rather than by an ioctl.
    formats: Vec<u32>,
    /// `COLOR_ENCODING` and `COLOR_RANGE`, if this plane has them. Measured on
    /// this board: plane 73 carries both, as enums — BT.601/BT.709/BT.2020 and
    /// limited/full — and both start on the first of each.
    encoding: Option<control::property::Handle>,
    range: Option<control::property::Handle>,
    /// `EOTF`, the transfer curve the display controller should read the
    /// frame with. Written on every film, including an SDR one, because the
    /// value that is already there came from whatever played last.
    eotf: Option<control::property::Handle>,
    /// `zpos`, so the film can be put under the interface rather than over it.
    depth: Option<control::property::Handle>,
    /// What an atomic update of this plane writes. `None` on a plane that does
    /// not publish them, which is then never used.
    props: Option<PlaneProps>,
    sunk: std::cell::Cell<bool>,
    /// What was last set, so a property is not written on every frame.
    applied: std::cell::Cell<Option<Colour>>,
    pub name: String,
}

impl VideoPlane {
    /// Every plane that could carry a film on `crtc`, best first.
    ///
    /// Which one the video port will actually accept is not knowable from here.
    /// The port publishes it — `CRTC 89 PLANE_MASK value: 5`, meaning Cluster0
    /// and Esmart0 — and each plane publishes its own name — `plane 73 NAME
    /// Esmart0-win0=0x100` — but both are *bitmask* properties, and this
    /// DRM crate keeps the names only for enums. Taking the first plane that
    /// merely says `possible_crtcs` includes us picked Esmart1, which this port
    /// does not drive.
    ///
    /// So the list is ordered by what a plane can do and the answer is settled
    /// by using it: a plane the port does not drive refuses `SetPlane`, and the
    /// next candidate is tried. That is one ioctl on the first frame of the
    /// first film, and it cannot be wrong about the hardware the way a name
    /// match can.
    ///
    /// `type` is not consulted at all: this driver reports Esmart0 as a Cursor
    /// plane, which it plainly is not — twenty-one formats and a scaler.
    pub fn candidates<D: ControlDevice>(
        device: &D,
        crtc: control::crtc::Handle,
        primary: Option<control::plane::Handle>,
    ) -> Vec<Self> {
        let Ok(planes) = device.plane_handles() else {
            return Vec::new();
        };
        let Ok(resources) = device.resource_handles() else {
            return Vec::new();
        };

        let mut found: Vec<Self> = Vec::new();
        for handle in planes {
            if Some(handle) == primary {
                continue;
            }
            let Ok(info) = device.get_plane(handle) else {
                continue;
            };
            // Which CRTCs a plane may be used on is a bitmask over the resource
            // list's own order, so the resource list is what decodes it — the
            // same way an encoder's possible_crtcs is read.
            if !resources
                .filter_crtcs(info.possible_crtcs())
                .contains(&crtc)
            {
                continue;
            }
            let formats: Vec<u32> = info.formats().to_vec();
            if !formats.contains(&(DrmFourcc::Nv12 as u32)) {
                continue;
            }
            let (encoding, range, eotf) = colour_properties(device, handle);
            let depth = zpos(device, handle).map(|(property, _)| property);
            let Some(props) = plane_props(device, handle) else {
                continue;
            };
            found.push(Self {
                plane: handle,
                formats,
                encoding,
                range,
                eotf,
                depth,
                props: Some(props),
                sunk: std::cell::Cell::new(false),
                applied: std::cell::Cell::new(None),
                name: format!("{handle:?}"),
            });
        }

        // Ten-bit first: an HEVC Main 10 film is what this appliance is for, and
        // a plane that cannot take NV15 would mean converting one.
        found.sort_by_key(|plane| !plane.accepts(DrmFourcc::Nv15));
        found
    }

    pub fn accepts(&self, fourcc: DrmFourcc) -> bool {
        self.formats.contains(&(fourcc as u32))
    }

    /// Imports a frame's dma-buf planes and gives them a framebuffer.
    pub fn import<D: ControlDevice>(
        &self,
        device: &D,
        frame: &Frame,
        dma_bufs: &[BorrowedFd<'_>],
    ) -> Result<Imported<D>, String>
    where
        D: Clone,
    {
        if !self.accepts(frame.fourcc) {
            return Err(format!("{} cannot show {:?}", self.name, frame.fourcc));
        }
        if dma_bufs.is_empty() || frame.planes == 0 || frame.planes > 4 {
            return Err(format!(
                "a frame arrived with {} planes and {} file descriptors",
                frame.planes,
                dma_bufs.len()
            ));
        }

        // One buffer object per plane, or — much more usually — one buffer with
        // the luma and the chroma at different offsets inside it, which is what
        // MPP produces. Importing the same descriptor twice gives the same GEM
        // handle back, so the handle is what is counted: the framebuffer needs
        // one per plane, and this must close each object exactly once.
        let mut handles: Vec<BufferHandle> = Vec::new();
        let mut slots: [Option<BufferHandle>; 4] = [None; 4];
        for plane in 0..frame.planes {
            let fd = dma_bufs.get(plane).copied().unwrap_or(dma_bufs[0]);
            match device.prime_fd_to_buffer(fd) {
                Ok(handle) => {
                    if !handles.contains(&handle) {
                        handles.push(handle);
                    }
                    slots[plane] = Some(handle);
                }
                Err(e) => {
                    for handle in &handles {
                        let _ = device.close_buffer(*handle);
                    }
                    return Err(format!(
                        "DRM_IOCTL_PRIME_FD_TO_HANDLE(video plane {plane}): {e}"
                    ));
                }
            }
        }

        let planar = Planar {
            fourcc: frame.fourcc,
            size: (frame.width, frame.height),
            pitches: frame.pitches,
            handles: slots,
            offsets: frame.offsets,
            modifier: DrmModifier::from(frame.modifier),
        };

        match device.add_planar_framebuffer(&planar, control::FbCmd2Flags::MODIFIERS) {
            Ok(framebuffer) => Ok(Imported {
                device: device.clone(),
                framebuffer,
                handles,
            }),
            Err(e) => {
                for handle in &handles {
                    let _ = device.close_buffer(*handle);
                }
                Err(format!("DRM_IOCTL_MODE_ADDFB2(video frame): {e}"))
            }
        }
    }

    /// Puts an imported frame into an atomic request: the framebuffer, the
    /// scaler's source and destination, and -- when they differ from what the
    /// plane last carried -- the colour properties and the stacking order.
    ///
    /// Atomic rather than `drmModeSetPlane`, and in the same request as the
    /// interface's own frame whenever the interface has one: a legacy
    /// `SetPlane` waits for its own vblank, and so did the interface's page
    /// flip, one after the other on the one thread that holds master. Measured
    /// on the Plus with a 24p film at 24 Hz, ten remote presses: 8 frames late,
    /// the longest gap 110 ms. One commit per vblank carries both.
    pub fn stage<D: ControlDevice>(
        &self,
        request: &mut control::atomic::AtomicModeReq,
        crtc: control::crtc::Handle,
        imported: &Imported<D>,
        frame: &FrameInfo,
        dst: Rect,
    ) -> bool {
        use control::property::Value;
        let Some(props) = self.props else { return false };
        request.add_property(self.plane, props.fb, Value::Framebuffer(Some(imported.framebuffer)));
        request.add_property(self.plane, props.crtc, Value::CRTC(Some(crtc)));
        let src = [0, 0, u64::from(frame.width) << 16, u64::from(frame.height) << 16];
        for (handle, value) in props.src.iter().zip(src) {
            request.add_property(self.plane, *handle, Value::UnsignedRange(value));
        }
        request.add_property(self.plane, props.dst[0], Value::SignedRange(dst.x.into()));
        request.add_property(self.plane, props.dst[1], Value::SignedRange(dst.y.into()));
        request.add_property(self.plane, props.dst[2], Value::UnsignedRange(dst.width.into()));
        request.add_property(self.plane, props.dst[3], Value::UnsignedRange(dst.height.into()));
        if self.applied.get() != Some(frame.colour) {
            for (property, value) in [
                (self.encoding, frame.colour.encoding),
                (self.range, frame.colour.range),
                // Written even when it is zero: a film that is not HDR has
                // to say so, or it is shown through the last film's curve.
                (self.eotf, frame.colour.eotf),
            ] {
                if let Some(property) = property {
                    request.add_property(self.plane, property, Value::UnsignedRange(value));
                }
            }
        }
        if !self.sunk.get()
            && let Some(depth) = self.depth
        {
            request.add_property(self.plane, depth, Value::UnsignedRange(0));
        }
        true
    }

    /// The request that carried `stage`'s properties was committed.
    fn staged(&self, colour: Colour) {
        self.applied.set(Some(colour));
        if self.depth.is_some() {
            self.sunk.set(true);
        }
    }

    /// The bottom of the stack, so the interface is drawn over the film.
    ///
    /// The other half of `raise_interface`; done here because a plane only has
    /// a stacking order while it is being used.
    fn sink_below_the_interface<D: ControlDevice>(&self, device: &D) {
        if self.sunk.get() {
            return;
        }
        let Some(property) = self.depth else { return };
        match device.set_property(self.plane, property, 0) {
            Ok(()) => self.sunk.set(true),
            Err(e) => eprintln!("mediabox-tv.video zpos: {e}"),
        }
    }

    /// Tell the plane which matrix and which range the frame uses.
    ///
    /// Without this the display controller converts every film as BT.601
    /// limited. Measured on the appliance while a 4K film was on the plane:
    /// `Esmart0-win0 color-encoding[BT.601] color-range[Limited]` under an
    /// HD picture, which is a visible shift in greens and reds.
    fn apply_colour<D: ControlDevice>(&self, device: &D, colour: Colour) {
        if self.applied.get() == Some(colour) {
            return;
        }
        let mut done = true;
        if let Some(property) = self.encoding
            && let Err(e) = device.set_property(self.plane, property, colour.encoding)
        {
            eprintln!("mediabox-tv.video COLOR_ENCODING: {e}");
            done = false;
        }
        if let Some(property) = self.range
            && let Err(e) = device.set_property(self.plane, property, colour.range)
        {
            eprintln!("mediabox-tv.video COLOR_RANGE: {e}");
            done = false;
        }
        // Written even when it is zero. A film that is not HDR has to say so,
        // or it is shown through the curve the last film left behind.
        if let Some(property) = self.eotf
            && let Err(e) = device.set_property(self.plane, property, colour.eotf)
        {
            eprintln!("mediabox-tv.video EOTF: {e}");
            done = false;
        }
        if done {
            self.applied.set(Some(colour));
        }
    }

    /// Takes the video off the panel. The interface is drawing underneath it
    /// and does not need to be told.
    pub fn hide<D: ControlDevice>(&self, device: &D, crtc: control::crtc::Handle) {
        self.applied.set(None);
        self.sunk.set(false);
        // A null framebuffer is what turns a plane off; the CRTC is named only
        // because the ioctl has a field for it.
        if let Err(e) = device.set_plane(self.plane, crtc, None, 0, (0, 0, 0, 0), (0, 0, 0, 0)) {
            eprintln!("mediabox-tv.video could not clear the plane: {e}");
        }
    }
}

impl<D: ControlDevice + AsFd> Imported<D> {
    /// The framebuffer, for a caller that wants to say so in the journal.
    pub fn framebuffer(&self) -> control::framebuffer::Handle {
        self.framebuffer
    }
}

// ---------------------------------------------------------------------------
// Where the frames come from.
//
// The decoder is a separate process, and it has to be: it is mpv, built on the
// appliance against MediaBox's own Rockchip media runtime, and the interface is
// a cross-compiled Rust binary that must not be. What the two
// share is one unix socket and the kernel's own buffer sharing — the decoder
// sends the dma-buf file descriptors of a frame it has already decoded, the
// interface imports them on the card it holds master on, and nothing between
// the two is a copy.
//
// The interface listens rather than connects, because the interface is what
// outlives a film.
// ---------------------------------------------------------------------------

use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// Where the socket lives unless the environment says otherwise. Under the
/// unit's own RuntimeDirectory, which systemd creates and removes with it.
pub const DEFAULT_SOCKET: &str = "/run/mediabox-ui/video.sock";

/// "MBV1", read back on a little-endian machine. Both ends of this socket are
/// on the same one, so the wire is native order throughout.
const MAGIC: u32 = 0x3156_424d;
const KIND_FRAME: u32 = 0;
const KIND_STOP: u32 = 1;
const KIND_RELEASE: u32 = 2;
const KIND_HDR: u32 = 3;
const KIND_SUBTITLE: u32 = 4;
/// A frame with when it is due: `MESSAGE` bytes as `KIND_FRAME`, then the
/// due time, 8 bytes, in the same `sendmsg` (mediabox-live).
const KIND_FRAME_TIMED: u32 = 5;

/// One fixed-size message per `sendmsg`, so a frame and its descriptors can
/// never be split across reads:
///
/// ```text
///  0  magic          16  fourcc      32  pitches[4]   64  modifier
///  4  kind           20  width       48  offsets[4]   72  descriptors
///  8  id             24  height                       76  flags
///                    28  planes                       80  display width
///                                                     84  display height
/// ```
const MESSAGE: usize = 88;
/// A `KIND_FRAME_TIMED` message carries 8 bytes more: when it is due on the
/// panel, CLOCK_MONOTONIC nanoseconds (mediabox-live; vo_mediabox sends
/// none).
///
/// The socket is a stream: reads do not keep messages apart. Every message
/// is read `MESSAGE` bytes at a time and the 8 more only when its kind says
/// they are there. Read 96 at a time, an 88-byte HDR message followed by a
/// 96-byte frame came as one read of the HDR and the frame's first 8 bytes,
/// and the frame's other 88 were thrown away as no message: the first frame
/// of a channel never shown and never given back, and the player waiting
/// 500 ms for it at the next channel (measured 2026-10-31, NTV HD).
const TIMED: usize = MESSAGE + 8;
const REPLY: usize = 16;
const MAX_FDS: usize = 4;

/// How many shown frames to hold on to.
///
/// `drmModeSetPlane` takes effect at the next vertical blank, so the frame
/// before the one just handed over may still be on the wire. Releasing it back
/// to the decoder at that moment invites the decoder to draw the next picture
/// into a buffer the panel is reading. Two deep: showing N releases N-2, which
/// stopped being scanned out when N-1 went up.
const IN_FLIGHT: usize = 2;

/// What arrived on the socket.
pub enum Incoming {
    Frame {
        id: u64,
        frame: Frame,
        display: (u32, u32),
        fds: Vec<OwnedFd>,
        /// When it is due on the panel, CLOCK_MONOTONIC ns; none: now.
        due_ns: Option<u64>,
    },
    /// What the film says about itself; sent once per configuration.
    Hdr(HdrStatic),
    /// A picture subtitle came on, changed or went (`None`).
    Subtitle(Option<SubtitleImage>),
    /// The player is finished with the plane but is still there.
    Stop,
    /// The player went away.
    Gone,
}

/// A picture subtitle (a Blu-ray's PGS, a DVD's VobSub), drawn by the player
/// into premultiplied RGBA because it has no text for this interface to draw.
///
/// `canvas` is the film's own picture at the size the player drew it; `x`,
/// `y`, `width` and `height` are the part of it that holds anything. The
/// interface puts that part where it falls on the picture, scaled, as Kodi
/// does (OverlayRenderer.cpp, ALIGN_VIDEO).
#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleImage {
    pub canvas: (u32, u32),
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// RGBA, premultiplied, `width * 4` bytes a row.
    pub rgba: Vec<u8>,
}

/// Where a picture subtitle goes, as fractions of the panel: what the
/// interface's own layout needs, whatever its scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SubtitlePlacement {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// The most a subtitle's picture may be: a whole 1920x1080 canvas.
const SUBTITLE_MAX_BYTES: u64 = 1920 * 1080 * 4;

/// The picture the player sent: `width` by `height` premultiplied BGRA rows,
/// `stride` bytes apart, in a memfd.
///
/// ```text
///  8  canvas width    16  x    24  width     32  stride
/// 12  canvas height   20  y    28  height    72  descriptors
/// ```
fn read_subtitle(bytes: &[u8; MESSAGE], fds: &[OwnedFd]) -> Result<Option<SubtitleImage>, String> {
    use std::os::unix::fs::FileExt;

    let (width, height, stride) = (u32_at(bytes, 24), u32_at(bytes, 28), u32_at(bytes, 32));
    let canvas = (u32_at(bytes, 8), u32_at(bytes, 12));
    if width == 0 || height == 0 {
        return Ok(None);
    }
    let Some(fd) = fds.first() else {
        return Err("a subtitle picture with no descriptor".into());
    };
    let row = u64::from(width) * 4;
    let size = u64::from(stride) * u64::from(height);
    if u64::from(stride) < row || size > SUBTITLE_MAX_BYTES * 2 || row * u64::from(height) > SUBTITLE_MAX_BYTES {
        return Err(format!("a subtitle picture {width}x{height} stride {stride}"));
    }
    let file = std::fs::File::from(fd.try_clone().map_err(|error| error.to_string())?);
    let mut raw = vec![0u8; size as usize];
    file.read_exact_at(&mut raw, 0).map_err(|error| error.to_string())?;
    let mut rgba = Vec::with_capacity((row * u64::from(height)) as usize);
    for line in raw.chunks_exact(stride as usize) {
        for pixel in line[..row as usize].chunks_exact(4) {
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
    }
    Ok(Some(SubtitleImage {
        canvas,
        x: u32_at(bytes, 16),
        y: u32_at(bytes, 20),
        width,
        height,
        rgba,
    }))
}

/// Where `image` falls on a panel `panel` wide and high, the picture being at
/// `picture` on it.
pub fn place_subtitle(image: &SubtitleImage, picture: Rect, panel: (u32, u32)) -> Option<SubtitlePlacement> {
    let (canvas_w, canvas_h) = image.canvas;
    if canvas_w == 0 || canvas_h == 0 || panel.0 == 0 || panel.1 == 0 {
        return None;
    }
    let scale_x = picture.width as f32 / canvas_w as f32;
    let scale_y = picture.height as f32 / canvas_h as f32;
    Some(SubtitlePlacement {
        x: (picture.x as f32 + image.x as f32 * scale_x) / panel.0 as f32,
        y: (picture.y as f32 + image.y as f32 * scale_y) / panel.1 as f32,
        width: image.width as f32 * scale_x / panel.0 as f32,
        height: image.height as f32 * scale_y / panel.1 as f32,
    })
}

fn u16_at(bytes: &[u8; MESSAGE], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8; MESSAGE], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
}

fn u64_at(bytes: &[u8; MESSAGE], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("eight bytes"))
}

fn quad_at(bytes: &[u8; MESSAGE], offset: usize) -> [u32; 4] {
    [
        u32_at(bytes, offset),
        u32_at(bytes, offset + 4),
        u32_at(bytes, offset + 8),
        u32_at(bytes, offset + 12),
    ]
}

/// CLOCK_MONOTONIC, ns: the clock of the kernel's vblank timestamps and of
/// the player's due times.
fn monotonic_ns() -> u64 {
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: a valid clock id and a timespec on the stack.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
}

/// One message and whatever descriptors came with it.
///
/// `Ok(None)` means nothing was waiting; `Ok(Some((0, _)))` means the peer has
/// gone.
fn recv_message(fd: RawFd, bytes: &mut [u8; TIMED]) -> io::Result<Option<(usize, Vec<OwnedFd>)>> {
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: MESSAGE,
    };
    let space = unsafe { libc::CMSG_SPACE((MAX_FDS * size_of::<RawFd>()) as u32) } as usize;
    let mut control = vec![0u8; space];

    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    header.msg_iov = &mut iov;
    header.msg_iovlen = 1;
    header.msg_control = control.as_mut_ptr().cast();
    header.msg_controllen = space as _;

    // CLOEXEC on arrival: a descriptor that reached this process by accident
    // must not reach anything this process starts.
    let read = unsafe { libc::recvmsg(fd, &mut header, libc::MSG_CMSG_CLOEXEC) };
    if read < 0 {
        let error = io::Error::last_os_error();
        return match error.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => Ok(None),
            _ => Err(error),
        };
    }

    let mut fds = Vec::new();
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&header);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let payload = (*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
                for index in 0..(payload / size_of::<RawFd>()) {
                    fds.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(
                        data.add(index),
                    )));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&header, cmsg);
        }
    }
    let mut read = read as usize;
    // The due time of a timed frame: in the same `sendmsg`, so here already.
    if read == MESSAGE && u32_at(bytes[..MESSAGE].try_into().expect("MESSAGE"), 4) == KIND_FRAME_TIMED {
        let more = unsafe { libc::recv(fd, bytes[MESSAGE..].as_mut_ptr().cast(), TIMED - MESSAGE, libc::MSG_DONTWAIT) };
        if more > 0 {
            read += more as usize;
        }
    }
    Ok(Some((read, fds)))
}

/// The socket the player draws through.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    client: Option<UnixStream>,
    /// The connected player's process id (SO_PEERCRED), for the timeline:
    /// a frame's id is the player's own, unique with the player's process.
    peer: u32,
}

/// The process at the other end of a Unix socket.
fn peer_pid(stream: &UnixStream) -> u32 {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: a connected socket and a ucred on the stack with its length.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc == 0 { cred.pid as u32 } else { 0 }
}

impl Server {
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // A socket left by a previous run is a file, not a listener; bind
        // refuses to replace it.
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        // The player runs as root in a transient unit of its own. Nothing else
        // on this appliance has any business putting pictures on the panel.
        let _ = std::fs::set_permissions(&path, PermissionsExt::from_mode(0o600));
        Ok(Self {
            listener,
            path,
            client: None,
            peer: 0,
        })
    }

    /// The connected player's process id; 0 with none.
    pub fn peer(&self) -> u32 {
        self.peer
    }

    /// The descriptor to wait on: whoever is connected, or the door.
    pub fn poll_fd(&self) -> RawFd {
        match &self.client {
            Some(client) => client.as_raw_fd(),
            None => self.listener.as_raw_fd(),
        }
    }

    pub fn connected(&self) -> bool {
        self.client.is_some()
    }

    /// Everything waiting on the socket, in order.
    pub fn pump(&mut self) -> Vec<Incoming> {
        let mut received = Vec::new();
        let Some(client) = self.client.as_ref() else {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(true);
                    eprintln!("mediabox-tv.video player connected");
                    self.peer = peer_pid(&stream);
                    mediabox_core::timeline!("client", "pid={}", self.peer);
                    self.client = Some(stream);
                }
                Err(error) if error.kind() != io::ErrorKind::WouldBlock => {
                    eprintln!("mediabox-tv.video accept: {error}");
                }
                Err(_) => {}
            }
            return received;
        };

        let fd = client.as_raw_fd();
        loop {
            let mut wire = [0u8; TIMED];
            match recv_message(fd, &mut wire) {
                Ok(None) => return received,
                Err(error) => {
                    // A player that exits without closing politely resets the
                    // connection, and that is the ordinary end of a film rather
                    // than a fault worth an error line.
                    if error.kind() != io::ErrorKind::ConnectionReset {
                        eprintln!("mediabox-tv.video recvmsg: {error}");
                    }
                    self.client = None;
                    received.push(Incoming::Gone);
                    return received;
                }
                Ok(Some((0, _))) => {
                    eprintln!("mediabox-tv.video player disconnected");
                    self.client = None;
                    received.push(Incoming::Gone);
                    return received;
                }
                Ok(Some((read, fds))) => {
                    let bytes: [u8; MESSAGE] = wire[..MESSAGE].try_into().expect("TIMED is longer");
                    let timed = u32_at(&bytes, 4) == KIND_FRAME_TIMED;
                    let due_ns = (timed && read == TIMED).then(|| u64::from_le_bytes(wire[MESSAGE..].try_into().expect("8 bytes")));
                    if read != if timed { TIMED } else { MESSAGE } || u32_at(&bytes, 0) != MAGIC {
                        eprintln!("mediabox-tv.video ignoring a {read} byte message");
                        mediabox_core::timeline!("wire-ignored", "bytes={read} kind={}", u32_at(&bytes, 4));
                        continue;
                    }
                    match u32_at(&bytes, 4) {
                        KIND_STOP => received.push(Incoming::Stop),
                        KIND_SUBTITLE => match read_subtitle(&bytes, &fds) {
                            Ok(image) => received.push(Incoming::Subtitle(image)),
                            Err(error) => eprintln!("mediabox-tv.video {error}"),
                        },
                        KIND_HDR => {
                            let mut primaries = [0u16; 8];
                            for (i, slot) in primaries.iter_mut().enumerate() {
                                *slot = u16_at(&bytes, 12 + i * 2);
                            }
                            received.push(Incoming::Hdr(HdrStatic {
                                eotf: u32_at(&bytes, 8) as u8,
                                primaries,
                                max_luminance: u16_at(&bytes, 28),
                                min_luminance: u16_at(&bytes, 30),
                                max_cll: u16_at(&bytes, 32),
                                max_fall: u16_at(&bytes, 34),
                            }));
                        }
                        KIND_FRAME | KIND_FRAME_TIMED => {
                            let Ok(fourcc) = DrmFourcc::try_from(u32_at(&bytes, 16)) else {
                                eprintln!(
                                    "mediabox-tv.video unknown format {:#x}",
                                    u32_at(&bytes, 16)
                                );
                                continue;
                            };
                            received.push(Incoming::Frame {
                                id: u64_at(&bytes, 8),
                                frame: Frame {
                                    fourcc,
                                    width: u32_at(&bytes, 20),
                                    height: u32_at(&bytes, 24),
                                    planes: u32_at(&bytes, 28) as usize,
                                    pitches: quad_at(&bytes, 32),
                                    offsets: quad_at(&bytes, 48),
                                    modifier: u64_at(&bytes, 64),
                                    colour: Colour {
                                        encoding: u64::from(u32_at(&bytes, 76) & 0xf),
                                        range: u64::from((u32_at(&bytes, 76) >> 4) & 0x1),
                                        eotf: u64::from((u32_at(&bytes, 76) >> 8) & 0xf),
                                    },
                                },
                                display: (u32_at(&bytes, 80), u32_at(&bytes, 84)),
                                fds,
                                due_ns,
                            });
                        }
                        other => eprintln!("mediabox-tv.video unknown message {other}"),
                    }
                }
            }
        }
    }

    /// Tell the player it may draw into that frame's buffer again.
    pub fn release(&mut self, id: u64) {
        let Some(client) = self.client.as_ref() else {
            return;
        };
        let mut reply = [0u8; REPLY];
        reply[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        reply[4..8].copy_from_slice(&KIND_RELEASE.to_le_bytes());
        reply[8..16].copy_from_slice(&id.to_le_bytes());
        let sent = unsafe {
            libc::send(
                client.as_raw_fd(),
                reply.as_ptr().cast(),
                REPLY,
                libc::MSG_NOSIGNAL,
            )
        };
        if sent < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::WouldBlock {
                eprintln!("mediabox-tv.video release: {error}");
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

struct Pending<D: ControlDevice> {
    id: u64,
    due_ns: Option<u64>,
    /// When it reached this process (CLOCK_MONOTONIC ns).
    arrived_ns: u64,
    imported: Imported<D>,
    info: FrameInfo,
    dst: Rect,
    panel: (u32, u32),
}

/// The film on the panel: the socket, the plane it settled on, and the frames
/// the display controller has not finished with.
pub struct Sink<D: ControlDevice + Clone> {
    server: Server,
    /// Which candidate the video port actually accepted. Settled once, by
    /// using it — see `VideoPlane::candidates`.
    chosen: Option<usize>,
    shown: VecDeque<(u64, Imported<D>)>,
    /// The frames imported and not yet committed, oldest first. One with no
    /// time replaces all before it (it is to be shown now); timed ones wait
    /// for their vblank (`due`), and of those due by one vblank only the
    /// newest is shown, the others going straight back to the player: they
    /// were never on the panel (Android's SurfaceView: "if multiple buffers
    /// are sent to the Surface to be rendered at the same VSYNC, the last one
    /// will be shown, and the other ones will be dropped").
    pending: VecDeque<Pending<D>>,
    /// The last vblank, CLOCK_MONOTONIC ns, and the refresh period: when the
    /// next ones fall. None until the kernel has said, and again after a
    /// mode change: the grid of the mode before is not this one's.
    vblank: Option<(u64, u64)>,
    /// The kernel will not say when its vblanks are (the vblank request was
    /// refused): timed frames then go up as they come, as they always did.
    no_vblank: bool,
    /// Added to every due time of the film now playing, fixed at its first
    /// timed frame (`snap_offset`): its frames fall on the vblanks, not
    /// between two.
    snap: Option<i64>,
    /// The snap the film started with (`track_snap` keeps within half a
    /// period of it), and the due time of the frame last committed, to be
    /// set against the vblank its page flip reports.
    snap_first: i64,
    shown_due: Option<u64>,
    /// The due time of the frame last committed, for the live monitor
    /// when its flip comes (`flipped`).
    flip_wait: Option<u64>,
    fault_staged: u64,
    fault_holding: bool,
    fault_held: Option<u64>,
    /// When the last frame arrived: whether the film is moving (`moving`).
    arrived: Option<std::time::Instant>,
    on: bool,
    refused: bool,
    frames: u64,
    /// The last thing the player said about the film it is sending. Cleared
    /// when the film stops, because the television must not be left being
    /// told it is receiving HDR by a player that has gone.
    hdr: Option<HdrStatic>,
    /// The picture subtitle on now, and where the film's picture is on the
    /// panel: the last frame's rectangle, and the panel's size.
    subtitle: Option<SubtitleImage>,
    picture: Option<(Rect, (u32, u32))>,
    /// Counts every change to either, for whoever draws the subtitle.
    subtitle_changes: u64,
}

impl<D: ControlDevice + Clone> Sink<D> {
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            server: Server::bind(path)?,
            chosen: None,
            shown: VecDeque::new(),
            pending: VecDeque::new(),
            vblank: None,
            no_vblank: false,
            snap: None,
            snap_first: 0,
            shown_due: None,
            flip_wait: None,
            fault_staged: 0,
            fault_holding: false,
            fault_held: None,
            arrived: None,
            on: false,
            refused: false,
            frames: 0,
            hdr: None,
            subtitle: None,
            picture: None,
            subtitle_changes: 0,
        })
    }

    pub fn poll_fd(&self) -> RawFd {
        self.server.poll_fd()
    }

    /// Whether there is a film on the panel right now.
    /// What the film asked the television to be told, while one is playing.
    pub fn hdr(&self) -> Option<HdrStatic> {
        self.on.then_some(self.hdr).flatten()
    }

    pub fn showing(&self) -> bool {
        self.on
    }

    /// A film is on the panel and its frames are still coming: the last one
    /// arrived within `within`. False while it is paused.
    pub fn moving(&self, within: std::time::Duration) -> bool {
        self.on && self.arrived.is_some_and(|at| at.elapsed() < within)
    }

    /// When the last frame of the film arrived.
    pub fn last_arrival(&self) -> Option<std::time::Instant> {
        self.arrived
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The connected player's process id, for the timeline.
    pub fn peer(&self) -> u32 {
        self.server.peer()
    }

    /// The picture subtitle to draw and where, with a number that changes
    /// whenever either does.
    pub fn subtitle(&self) -> (u64, Option<(&SubtitleImage, SubtitlePlacement)>) {
        let shown = match (&self.subtitle, self.picture) {
            (Some(image), Some((picture, panel))) if self.on => {
                place_subtitle(image, picture, panel).map(|placement| (image, placement))
            }
            _ => None,
        };
        (self.subtitle_changes, shown)
    }

    fn set_subtitle(&mut self, image: Option<SubtitleImage>) {
        if self.subtitle != image {
            self.subtitle = image;
            self.subtitle_changes += 1;
        }
    }

    /// Reads whatever is waiting and puts it on the panel.
    ///
    /// `into` is the whole area the film may use, in the panel's own pixels;
    /// the frame is fitted inside it at the aspect the decoder asked for and
    /// the display controller's own scaler does the rest.
    pub fn pump(
        &mut self,
        device: &D,
        crtc: control::crtc::Handle,
        planes: &[VideoPlane],
        into: Rect,
    ) {
        for message in self.server.pump() {
            match message {
                Incoming::Hdr(hdr) => self.hdr = Some(hdr),
                Incoming::Subtitle(image) => self.set_subtitle(image),
                Incoming::Stop | Incoming::Gone => self.clear(device, crtc, planes),
                Incoming::Frame {
                    id,
                    frame,
                    display,
                    fds,
                    due_ns,
                } => {
                    self.present(device, crtc, planes, &frame, display, into, id, &fds, due_ns);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn present(
        &mut self,
        device: &D,
        _crtc: control::crtc::Handle,
        planes: &[VideoPlane],
        frame: &Frame,
        display: (u32, u32),
        into: Rect,
        id: u64,
        fds: &[OwnedFd],
        due_ns: Option<u64>,
    ) {
        let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(AsFd::as_fd).collect();
        let dst = fit(display, frame, into);

        // The framebuffer does not depend on the plane, only on the plane
        // taking the format; which plane the port drives is settled when the
        // frame is first committed (`stage`).
        let order: Vec<usize> = match self.chosen {
            Some(index) => vec![index],
            None => (0..planes.len()).collect(),
        };
        let mut last = String::from("no plane on this video port would take the frame");
        for index in order {
            match planes[index].import(device, frame, &borrowed) {
                Ok(imported) => {
                    self.refused = false;
                    let info = FrameInfo {
                        fourcc: frame.fourcc,
                        width: frame.width,
                        height: frame.height,
                        colour: frame.colour,
                    };
                    self.arrived = Some(std::time::Instant::now());
                    let panel = (into.width, into.height);
                    let mut replaced = 0;
                    if due_ns.is_none() {
                        while let Some(old) = self.pending.pop_front() {
                            replaced = old.id;
                            mediabox_core::timeline!(
                                "unseen",
                                "pid={} id={} why=untimed-replaced",
                                self.server.peer(),
                                old.id
                            );
                            crate::monitor::presenter_dropped(crate::monitor::Drop::Other);
                            self.server.release(old.id);
                        }
                    }
                    let arrived_ns = monotonic_ns();
                    self.pending.push_back(Pending { id, due_ns, arrived_ns, imported, info, dst, panel });
                    self.settle_snap();
                    // How far ahead of its vblank's commit deadline it came:
                    // the player's margin, as the presenter receives it.
                    if let (Some(due), Some(snap), Some((last, period))) = (due_ns, self.snap, self.vblank) {
                        let deadline = own_vblank(due.saturating_add_signed(snap), last, period) as i64 - COMMIT_MARGIN_NS as i64;
                        crate::monitor::arrived(deadline - arrived_ns as i64);
                    }
                    mediabox_core::timeline!(
                        "arrive",
                        "pid={} id={id} due={} pending={}",
                        self.server.peer(),
                        due_ns.unwrap_or(0),
                        self.pending.len()
                    );
                    trace::event('A', id, replaced);
                    trace::event('D', id, due_ns.unwrap_or(0) / 1000);
                    return;
                }
                Err(error) => last = error,
            }
        }

        // Nothing took it. Say so once — a film dropping every frame would
        // otherwise fill the journal at the panel's refresh rate — and hand the
        // buffer straight back so the decoder does not stall waiting for it.
        if !self.refused {
            eprintln!("mediabox-tv.video {last}");
            self.refused = true;
        }
        self.server.release(id);
    }

    /// Whether a frame is waiting to be committed for the next vblank.
    pub fn has_pending(&self) -> bool {
        self.due_now().is_some()
    }

    /// The commit's page flip completed at `at_ns` (the kernel's
    /// timestamp): the frame last committed went up then (`monitor`).
    pub fn flipped(&mut self, at_ns: u64, period_ns: u64) {
        if let Some(due) = self.flip_wait.take() {
            crate::monitor::shown(due, at_ns, period_ns);
        }
    }

    /// A vblank happened at `at_ns` (the kernel's timestamp), the refresh
    /// being `period_ns`.
    pub fn vblank(&mut self, at_ns: u64, period_ns: u64) {
        if period_ns > 0 {
            self.vblank = Some((at_ns, period_ns));
            self.no_vblank = false;
            self.settle_snap();
            // The frame last committed was scanned out at this vblank.
            if let (Some(due), Some(snap)) = (self.shown_due.take(), self.snap) {
                let next = track_snap(snap, self.snap_first, due, at_ns, period_ns);
                if next != snap {
                    self.snap = Some(next);
                    mediabox_core::timeline!(
                        "resnap",
                        "pid={} offset={next} was={snap} first={}",
                        self.server.peer(),
                        self.snap_first
                    );
                }
            }
        }
    }

    /// The film's phase against the vblanks, from its first timed frame
    /// once the grid is known.
    fn settle_snap(&mut self) {
        if self.snap.is_some() {
            return;
        }
        let (Some((last, period)), Some(due)) = (self.vblank, self.pending.iter().find_map(|frame| frame.due_ns)) else {
            return;
        };
        let snap = snap_offset(due, last, period);
        self.snap = Some(snap);
        self.snap_first = snap;
        mediabox_core::timeline!("snap", "pid={} offset={snap} period={period}", self.server.peer());
    }

    /// The mode changed: the vblanks known so far are of the mode before.
    /// Until the kernel reports one of the new mode, a timed frame waits.
    pub fn forget_vblank(&mut self) {
        self.vblank = None;
        self.snap = None;
        mediabox_core::timeline!("vblank-forget");
    }

    /// A timed frame waits for the vblank grid, and the kernel has to be
    /// asked for it (`DRM_VBLANK_EVENT`).
    pub fn wants_vblank(&self) -> bool {
        self.vblank.is_none() && !self.no_vblank && self.pending.iter().any(|frame| frame.due_ns.is_some())
    }

    /// The kernel refused to report a vblank: frames go up as they come.
    pub fn vblank_unavailable(&mut self) {
        self.no_vblank = true;
    }

    /// The next vblank a commit made now still reaches, and the period.
    fn next_vblank(&self) -> Option<(u64, u64)> {
        self.next_vblank_at(monotonic_ns())
    }

    fn next_vblank_at(&self, now: u64) -> Option<(u64, u64)> {
        let (last, period) = self.vblank?;
        Some((next_vblank_after(now, last, period), period))
    }

    /// Which waiting frame goes up at the next vblank (`due_index`); none
    /// while a commit made now could still land on the vblank about to
    /// come (`racing`).
    fn due_now(&self) -> Option<usize> {
        self.due_at(monotonic_ns())
    }

    /// `due_now` at `now`: one reading of the clock for every part of the
    /// answer. Read again between them, a vblank passing in between gave
    /// two answers at once -- not due (racing it) and then, the race over,
    /// the frame's own vblank still to come -- and the loop slept through
    /// the frame's commit (measured 2026-11-01: the first frame of two
    /// openings in 40, the last look 5 and 12 us before the vblank).
    fn due_at(&self, now: u64) -> Option<usize> {
        if self.vblank.is_none() && self.no_vblank {
            return self.pending.len().checked_sub(1);
        }
        if let Some((last, period)) = self.vblank
            && racing(now, last, period).is_some()
        {
            return None;
        }
        let snap = self.snap.unwrap_or(0);
        let dues: Vec<Option<u64>> =
            self.pending.iter().map(|frame| frame.due_ns.map(|due| due.saturating_add_signed(snap))).collect();
        due_index(&dues, self.next_vblank_at(now))
    }

    /// Before the first frame of a film is up, what decides it, on the
    /// timeline (`first-wait`): the vblank grid, the snap, each waiting
    /// frame's due time against the next vblank.
    pub fn trace_first_wait(&self, flip_pending: bool, ui_shown: bool) {
        if self.on || self.pending.is_empty() || !mediabox_core::timeline::enabled() {
            return;
        }
        let now = monotonic_ns();
        let racing = self.vblank.and_then(|(last, period)| racing(now, last, period));
        let dues: Vec<String> = self
            .pending
            .iter()
            .map(|frame| frame.due_ns.map_or("-".into(), |due| due.saturating_add_signed(self.snap.unwrap_or(0)).to_string()))
            .collect();
        mediabox_core::timeline!(
            "first-wait",
            "vblank={:?} snap={:?} next={:?} racing={:?} due_now={:?} dues={} flip_pending={flip_pending} ui={ui_shown} no_vblank={}",
            self.vblank,
            self.snap,
            self.next_vblank(),
            racing,
            self.due_now(),
            dues.join(","),
            self.no_vblank
        );
    }

    /// How long until a waiting frame is due at a vblank, if one waits --
    /// zero when one is due now: the event loop sleeps no longer than that.
    pub fn until_due(&self) -> Option<std::time::Duration> {
        if self.pending.is_empty() {
            return None;
        }
        let now = monotonic_ns();
        if self.due_at(now).is_some() {
            return Some(std::time::Duration::ZERO);
        }
        if let Some((last, period)) = self.vblank
            && let Some(vblank) = racing(now, last, period)
        {
            // Right after the vblank about to come.
            return Some(std::time::Duration::from_nanos(vblank.saturating_sub(now) + RACE_AFTER_NS));
        }
        let (vblank, _) = self.next_vblank_at(now)?;
        // Past the vblank that comes first, the next one is reachable.
        Some(std::time::Duration::from_nanos(vblank.saturating_sub(now) + 1_000_000))
    }

    /// Adds the waiting frame, if there is one, to `request`, on the plane
    /// the port drives -- found the first time by asking the kernel
    /// (`TEST_ONLY`) about each candidate in turn. Returns the plane staged
    /// on, for `committed` to be told.
    pub fn stage(
        &mut self,
        device: &D,
        crtc: control::crtc::Handle,
        planes: &[VideoPlane],
        request: &mut control::atomic::AtomicModeReq,
    ) -> Option<usize> {
        // Frames due at the same vblank as a newer one go back unseen.
        let due = self.due_now()?;
        // MEDIABOX_TV_FAULT=hold=N: every Nth frame held back one vblank, on
        // purpose, to show the live monitor sees it. Unset, never.
        static HOLD: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        let hold = *HOLD.get_or_init(|| {
            let n = std::env::var("MEDIABOX_TV_FAULT")
                .ok()
                .and_then(|v| v.strip_prefix("hold=").and_then(|n| n.parse().ok()))
                .unwrap_or(0);
            if n > 0 {
                eprintln!("mediabox-tv.video: BİLEREK KUSUR (monitör sınaması): her {n}. kare bir vblank geç");
            }
            n
        });
        if hold > 0 && due == 0 {
            let vblank = self.next_vblank().map(|(at, _)| at);
            if self.fault_held.is_some() && self.fault_held == vblank {
                return None;
            }
            self.fault_held = None;
            if !self.fault_holding {
                self.fault_staged += 1;
                if self.fault_staged % hold == 0 {
                    self.fault_holding = true;
                    self.fault_held = vblank;
                    mediabox_core::timeline!("fault", "hold n={} vblank={}", self.fault_staged, vblank.unwrap_or(0));
                    return None;
                }
            }
            self.fault_holding = false;
        }
        for _ in 0..due {
            if let Some(old) = self.pending.pop_front() {
                mediabox_core::timeline!(
                    "unseen",
                    "pid={} id={} why=same-vblank due={} vblank={} arrived={}",
                    self.server.peer(),
                    old.id,
                    old.due_ns.unwrap_or(0),
                    self.next_vblank().map_or(0, |(vblank, _)| vblank),
                    old.arrived_ns
                );
                // Whose: it came after its own vblank's commit deadline
                // (the player's), or in time and the presenter did not
                // commit it there (this process's).
                // Or it shares its vblank with the frame after it: more
                // frames than vblanks, a rate the panel cannot show.
                let snap = self.snap.unwrap_or(0);
                let why = match (old.due_ns, self.vblank) {
                    (Some(due), Some((last, period))) => {
                        let vblank = own_vblank(due.saturating_add_signed(snap), last, period);
                        let next = self.pending.front().and_then(|next| next.due_ns).map(|d| own_vblank(d.saturating_add_signed(snap), last, period));
                        if next == Some(vblank) {
                            crate::monitor::Drop::Cadence
                        } else if old.arrived_ns + COMMIT_MARGIN_NS > vblank {
                            crate::monitor::Drop::CameLate
                        } else {
                            crate::monitor::Drop::NotCommitted
                        }
                    }
                    _ => crate::monitor::Drop::Other,
                };
                crate::monitor::presenter_dropped(why);
                trace::event('A', 0, old.id);
                self.server.release(old.id);
            }
        }
        let pending = self.pending.front()?;
        if self.chosen.is_none() {
            for (index, plane) in planes.iter().enumerate() {
                if !plane.accepts(pending.info.fourcc) {
                    continue;
                }
                let mut test = control::atomic::AtomicModeReq::new();
                if !plane.stage(&mut test, crtc, &pending.imported, &pending.info, pending.dst) {
                    continue;
                }
                let flags = control::AtomicCommitFlags::TEST_ONLY;
                if device.atomic_commit(flags, test).is_ok() {
                    eprintln!(
                        "mediabox-tv.video settled on {} {:?} {}x{} -> {}x{}+{}+{}",
                        plane.name,
                        pending.info.fourcc,
                        pending.info.width,
                        pending.info.height,
                        pending.dst.width,
                        pending.dst.height,
                        pending.dst.x,
                        pending.dst.y
                    );
                    self.chosen = Some(index);
                    break;
                }
            }
        }
        let Some(index) = self.chosen else {
            if !self.refused {
                eprintln!("mediabox-tv.video no plane on this video port would take the frame");
                self.refused = true;
            }
            let old = self.pending.pop_front()?;
            self.server.release(old.id);
            return None;
        };
        planes[index]
            .stage(request, crtc, &pending.imported, &pending.info, pending.dst)
            .then_some(index)
    }

    /// The request `stage` added the waiting frame to was committed: it is on
    /// its way to the panel, and the frames before the one the panel shows
    /// now go back to the player.
    /// Returns the frame's id.
    pub fn committed(&mut self, planes: &[VideoPlane], index: usize) -> Option<u64> {
        let pending = self.pending.pop_front()?;
        let id = pending.id;
        self.shown_due = pending.due_ns;
        self.flip_wait = pending.due_ns;
        trace::event('c', pending.id, 0);
        planes[index].staged(pending.info.colour);
        if !self.on || self.picture.map(|(picture, _)| picture) != Some(pending.dst) {
            self.picture = Some((pending.dst, pending.panel));
            self.subtitle_changes += 1;
        }
        self.on = true;
        self.frames += 1;
        pace::presented();
        self.shown.push_back((pending.id, pending.imported));
        while self.shown.len() > IN_FLIGHT {
            if let Some((old, _)) = self.shown.pop_front() {
                self.server.release(old);
            }
        }
        Some(id)
    }

    /// The plane and framebuffer of the film frame committed last: what the
    /// panel shows once that commit's flip has come (`platform`'s recovery
    /// of a lost flip event reads the plane against it).
    pub fn on_panel(&self, planes: &[VideoPlane]) -> Option<(control::plane::Handle, control::framebuffer::Handle)> {
        let index = self.chosen?;
        let (_, imported) = self.shown.back()?;
        Some((planes.get(index)?.plane, imported.framebuffer()))
    }

    /// The commit carrying the waiting frame was refused: it goes back.
    pub fn refused_commit(&mut self, error: &str) {
        if let Some(pending) = self.pending.pop_front() {
            mediabox_core::timeline!(
                "unseen",
                "pid={} id={} why=commit-refused",
                self.server.peer(),
                pending.id
            );
            crate::monitor::presenter_dropped(crate::monitor::Drop::Other);
            if !self.refused {
                eprintln!("mediabox-tv.video the frame's commit was refused: {error}");
                self.refused = true;
            }
            self.server.release(pending.id);
        }
    }

    /// Take the film off the panel and give every buffer back.
    pub fn clear(&mut self, device: &D, crtc: control::crtc::Handle, planes: &[VideoPlane]) {
        self.hdr = None;
        self.set_subtitle(None);
        if self.picture.take().is_some() {
            self.subtitle_changes += 1;
        }
        self.arrived = None;
        // The next film (the next channel) has a phase of its own.
        self.snap = None;
        while let Some(pending) = self.pending.pop_front() {
            mediabox_core::timeline!(
                "unseen",
                "pid={} id={} why=stop",
                self.server.peer(),
                pending.id
            );
            self.server.release(pending.id);
        }
        mediabox_core::timeline!("plane-off", "frames={}", self.frames);
        if let Some(index) = self.chosen
            && let Some(plane) = planes.get(index)
        {
            plane.hide(device, crtc);
        }
        while let Some((id, _)) = self.shown.pop_front() {
            self.server.release(id);
        }
        if self.on {
            eprintln!(
                "mediabox-tv.video plane released after {} frames",
                self.frames
            );
        }
        self.on = false;
        self.frames = 0;
    }
}

/// A commit wants a moment before its vblank.
const COMMIT_MARGIN_NS: u64 = 2_000_000;
/// How long after a vblank a held commit goes: past it, for certain.
const RACE_AFTER_NS: u64 = 500_000;

/// What to add to a film's due times so that its first timed frame, due at
/// `due`, is due on the vblank nearest it (on the grid of `last`): never
/// more than half a period either way. A film's frames are 20 ms apart and
/// the vblanks 20 ms apart, but its phase against them is wherever the
/// stream put it -- measured, 9.5 ms from a vblank, half a period: then the
/// panel's clock and the board's (19.998 ms against 20.000, measured) carry
/// the due times across the midpoint within seconds, and a frame is shown
/// twice. On the vblank, they have half a period to drift before that --
/// minutes. ExoPlayer's VideoFrameReleaseHelper snaps release times to the
/// vsync the same way.
fn snap_offset(due: u64, last: u64, period: u64) -> i64 {
    let from = i128::from(due) - i128::from(last);
    let period = i128::from(period);
    let nearest = (from + period / 2).div_euclid(period) * period;
    (nearest - from) as i64
}

/// How far a scanned frame may stray from its snapped time before the snap
/// follows it.
const TRACK_NS: i64 = 5_000_000;

/// The snap after a frame due at `due` (before the snap) was scanned out at
/// the vblank `shown`.
///
/// Fixed at the first frame, the snap put the film's frames on the vblanks,
/// but the panel's clock and the broadcast's part by some ppm (12 measured
/// on NTV, 2026-11-01): after 14 minutes the frames sat half way between
/// two vblanks and went up at one or the other by turns -- 43 skipped and
/// 44 shown twice in ten minutes. Followed, the frames stay on the vblanks;
/// each step is under half a period, so no frame changes vblank by it. Once
/// the snap has moved half a period from where it started, a whole period
/// is taken back: one frame shown twice, or one skipped, where the drift
/// makes that unavoidable (one in 28 minutes at 12 ppm), and picture stays
/// within half a period of sound. A frame scanned half a period or more
/// from its time went up late or early, not out of phase: it moves nothing.
fn track_snap(snap: i64, first: i64, due: u64, shown: u64, period: u64) -> i64 {
    let period = period as i64;
    let error = due as i64 + snap - shown as i64;
    if error.abs() >= period / 2 || error.abs() <= TRACK_NS {
        return snap;
    }
    let mut next = snap - error;
    if (next - first).abs() > period / 2 {
        next -= (next - first).signum() * period;
    }
    next
}

/// The vblank a commit made at `now` might or might not still catch: one
/// less than `COMMIT_MARGIN_NS` away. The kernel takes a commit up to the
/// vblank itself -- measured on the Plus (2026-10-31), commits 0.15 ms
/// before a vblank landed on it -- so a frame chosen for the vblank after
/// went up a period early, and when the frames' arrival drifted past the
/// vblank by 40 us the same choice landed where it was meant to: one frame
/// shown for two vblanks. In this window nothing is committed until the
/// vblank has passed; then the vblank a commit lands on is the one it was
/// chosen for.
fn racing(now: u64, last: u64, period: u64) -> Option<u64> {
    let previous = next_vblank_after(now, last, period) - period;
    (previous > now).then_some(previous)
}

/// The vblank a frame due at `due` (snapped) belongs to: the grid point of
/// `last` nearest it -- the one `due_index` puts it up at.
fn own_vblank(due: u64, last: u64, period: u64) -> u64 {
    // The first vblank V with due <= V + period/2, as `due_index` has it.
    let from = i128::from(due) - i128::from(last) - i128::from(period / 2);
    let period = i128::from(period.max(1));
    let first = -((-from).div_euclid(period)) * period;
    (i128::from(last) + first).max(0) as u64
}

/// The next vblank a commit made at `now` still reaches, from the last one
/// seen and the refresh period (all CLOCK_MONOTONIC ns): on the grid of the
/// last, however many vblanks have gone by unseen since.
fn next_vblank_after(now: u64, last: u64, period: u64) -> u64 {
    let ahead = (now + COMMIT_MARGIN_NS)
        .saturating_sub(last)
        .div_ceil(period)
        .max(1);
    last + ahead * period
}

/// Of the frames waiting, oldest first, by when each is due: the one to put
/// up at the next vblank -- the newest of those due nearer it than the one
/// after (ExoPlayer's VideoFrameReleaseHelper puts a frame on the vsync
/// closest to its time); the ones before it go back unseen. A frame with no
/// time is due at once. A timed frame with no vblank known yet waits for
/// one: put up at once it lands on whatever vblank comes, ahead of its time,
/// and the next frame -- on the grid -- then leaves it up for two (measured
/// on the first frame after a 60 -> 50 Hz change, 2026-10-31).
fn due_index(dues: &[Option<u64>], next: Option<(u64, u64)>) -> Option<usize> {
    dues.iter().rposition(|due| match (due, next) {
        (Some(due), Some((vblank, period))) => *due <= vblank + period / 2,
        (Some(_), None) => false,
        (None, _) => true,
    })
}

/// The largest rectangle of the decoder's aspect that fits in `into`.
///
/// The frame's own pixels are not the answer: a film is stored at 1920x1080 and
/// displayed at 2.39:1 as often as not, and the decoder is the one that knows.
/// `display` is what mpv calls d_w/d_h; a decoder that does not say falls back
/// to the stored size.
fn fit(display: (u32, u32), frame: &Frame, into: Rect) -> Rect {
    let (mut want_w, mut want_h) = display;
    if want_w == 0 || want_h == 0 {
        want_w = frame.width;
        want_h = frame.height;
    }
    if want_w == 0 || want_h == 0 || into.width == 0 || into.height == 0 {
        return into;
    }

    let by_width = u64::from(want_h) * u64::from(into.width) / u64::from(want_w);
    let (width, height) = if by_width <= u64::from(into.height) {
        (into.width, by_width as u32)
    } else {
        (
            (u64::from(want_w) * u64::from(into.height) / u64::from(want_h)) as u32,
            into.height,
        )
    };

    Rect {
        x: into.x + ((into.width - width) / 2) as i32,
        y: into.y + ((into.height - height) / 2) as i32,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32) -> Frame {
        Frame {
            fourcc: DrmFourcc::Nv12,
            width,
            height,
            pitches: [width, width, 0, 0],
            offsets: [0, 0, 0, 0],
            planes: 2,
            modifier: 0,
            colour: Colour {
                encoding: 1,
                range: 0,
                eotf: 0,
            },
        }
    }

    const PANEL: Rect = Rect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };

    /// The panel's own shape: the film fills it.
    const MS: u64 = 1_000_000;
    const P50: u64 = 20 * MS;
    /// 1001/60 ms.
    const P5994: u64 = 16_683_333;

    /// An hour of a film whose clock runs `ppm` apart from the panel's,
    /// vblank timestamps jittering by 50 us: how many frames change by
    /// other than one vblank from the one before, with the snap followed
    /// or fixed.
    fn slot_changes(ppm: f64, follow: bool) -> (usize, usize) {
        let frames = 3600 * 50;
        let period = P50 as i64;
        let first = 3_000_000i64;
        let mut snap = first;
        let mut last_slot: Option<i64> = None;
        let (mut changes, mut flip_flops, mut previous_change) = (0, 0, 0usize);
        let mut state = 12345u64;
        for k in 0..frames as i64 {
            let due = 1_000 * MS as i64 - first + (k as f64 * period as f64 * (1.0 + ppm * 1e-6)) as i64;
            // The vblank nearest the snapped due, ties to the earlier one,
            // on a grid known from timestamps that jitter.
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let jitter = (state >> 33) as i64 % 100_000 - 50_000;
            let at = due + snap - jitter;
            let slot = (at - period / 2 + period - 1).div_euclid(period);
            let shown = (slot * period + jitter) as u64;
            if let Some(last) = last_slot
                && slot - last != 1
            {
                changes += 1;
                if k as usize - previous_change < 50 * 60 {
                    flip_flops += 1;
                }
                previous_change = k as usize;
            }
            last_slot = Some(slot);
            if follow {
                snap = track_snap(snap, first, due as u64, shown, P50);
            }
        }
        (changes, flip_flops)
    }

    #[test]
    fn a_drifting_film_gets_one_repeat_per_period_of_drift_not_a_burst() {
        // 12 ppm for an hour: 43 ms, two periods of drift.
        let (changes, flip_flops) = slot_changes(12.0, true);
        assert!(changes <= 3, "{changes}");
        assert_eq!(flip_flops, 0);
        // 75 ppm (the board's clock against the panel's, measured): 270 ms.
        let (changes, flip_flops) = slot_changes(75.0, true);
        assert!(changes <= 15, "{changes}");
        assert_eq!(flip_flops, 0);
        // Fixed, the 12 ppm film goes up at one vblank or the other by turns
        // once half way between them.
        let (changes, flip_flops) = slot_changes(12.0, false);
        assert!(flip_flops > 10, "{changes} {flip_flops}");
    }

    #[test]
    fn a_frame_due_exactly_half_way_goes_up_at_the_earlier_vblank() {
        let next = Some((1_000 * MS, P50));
        assert_eq!(due_index(&[Some(1_010 * MS)], next), Some(0));
        assert_eq!(due_index(&[Some(1_010 * MS + 1)], next), None);
    }

    #[test]
    fn of_two_frames_due_at_one_vblank_only_the_newer_goes_up() {
        let next = Some((1_000 * MS, P50));
        // The older one goes back unseen: index 1 is shown, 0 is released.
        assert_eq!(
            due_index(&[Some(999 * MS), Some(1_003 * MS), Some(1_030 * MS)], next),
            Some(1)
        );
    }

    #[test]
    fn a_frame_without_a_time_goes_at_once_and_a_timed_one_waits_for_the_grid() {
        assert_eq!(
            due_index(&[Some(5_000 * MS), None], Some((1_000 * MS, P50))),
            Some(1)
        );
        assert_eq!(due_index(&[Some(5_000 * MS)], None), None);
        assert_eq!(due_index(&[Some(5_000 * MS), None], None), Some(1));
        assert_eq!(due_index(&[], Some((0, P50))), None);
    }

    #[test]
    fn a_commit_too_late_for_a_vblank_reaches_the_one_after() {
        let last = 1_000 * MS;
        // 3 ms before the next vblank: in time.
        assert_eq!(
            next_vblank_after(last + P50 - 3 * MS, last, P50),
            last + P50
        );
        // Exactly the margin before it: still in time.
        assert_eq!(
            next_vblank_after(last + P50 - COMMIT_MARGIN_NS, last, P50),
            last + P50
        );
        // 1 ms before it: the one after.
        assert_eq!(
            next_vblank_after(last + P50 - MS, last, P50),
            last + 2 * P50
        );
        // Right after the last one: never the last one again.
        assert_eq!(next_vblank_after(last, last, P50), last + P50);
    }

    #[test]
    fn vblanks_gone_by_unseen_keep_the_grid() {
        let last = 1_000 * MS;
        // Ten vblanks with no page flip event (nothing committed).
        let now = last + 10 * P50 + 5 * MS;
        assert_eq!(next_vblank_after(now, last, P50), last + 11 * P50);
        assert_eq!(
            next_vblank_after(last + 10 * P5994 + MS, last, P5994),
            last + 11 * P5994
        );
    }

    /// Frames due one period apart, each arriving two periods ahead as the
    /// player sends them: every vblank shows exactly the next frame, at
    /// 50 Hz and at 59.94 Hz, whatever the phase of the due times against
    /// the vblanks.
    #[test]
    fn a_steady_cadence_puts_one_frame_on_every_vblank() {
        for period in [P50, P5994] {
            for phase in [0, period / 4, period / 2 - 1, period - 1] {
                let base = 1_000 * MS;
                let mut pending: Vec<(u64, u64)> = Vec::new();
                let mut shown = Vec::new();
                let mut unseen = 0;
                let mut next_frame = 0u64;
                for k in 0..200u64 {
                    let vblank = base + k * period;
                    let now = vblank - 5 * MS;
                    while base + phase + next_frame * period <= now + 2 * period {
                        pending.push((next_frame, base + phase + next_frame * period));
                        next_frame += 1;
                    }
                    let dues: Vec<Option<u64>> =
                        pending.iter().map(|(_, due)| Some(*due)).collect();
                    if let Some(index) = due_index(
                        &dues,
                        Some((next_vblank_after(now, vblank - period, period), period)),
                    ) {
                        unseen += index;
                        let rest = pending.split_off(index + 1);
                        shown.push(pending.last().unwrap().0);
                        pending = rest;
                    }
                }
                assert_eq!(unseen, 0, "period {period} phase {phase}");
                assert!(
                    shown.windows(2).all(|w| w[1] == w[0] + 1),
                    "period {period} phase {phase}: {shown:?}"
                );
            }
        }
    }

    /// The first frames after a mode change, in the order things happen:
    /// each frame arrives two periods before it is due (the player's render
    /// delay) and is looked at as it arrives (`pump_video`); the first one
    /// finds no vblank grid known and the kernel is asked; its event comes
    /// at the next vblank and the waiting frame is looked at again right
    /// after it. One commit is in flight at a time. From there every vblank
    /// shows the next frame -- the first one included, at the vblank nearest
    /// its time -- whatever the phase of the new mode's grid. Put up as it
    /// arrived, with no grid known, the first frame went up two vblanks
    /// early and stayed up for three.
    #[test]
    fn from_a_first_frame_with_no_vblank_known_every_frame_gets_one_vblank() {
        for period in [P50, P5994] {
            for phase in [0, period / 3, period / 2, period / 2 + 1, period - 1] {
                let grid = 10_000 * MS + phase; // the new mode's vblanks
                let at = |index: u64| grid + index * period;
                let first_due = 10_100 * MS + 7 * MS;
                let arrival = |n: u64| first_due + n * period - 2 * period;
                let mut pending: Vec<(u64, u64)> = Vec::new();
                let (mut known, mut asked): (Option<(u64, u64)>, bool) = (None, false);
                let mut in_flight: Option<u64> = None;
                let mut shown: Vec<(u64, u64)> = Vec::new(); // (frame, vblank index)
                let (mut unseen, mut next_frame) = (0, 0u64);
                // A commit made at `now`, if a frame is due: it lands on the
                // first vblank of the real grid it still reaches.
                let mut decide = |now: u64, pending: &mut Vec<(u64, u64)>, known: Option<(u64, u64)>, in_flight: &mut Option<u64>| {
                    if in_flight.is_some() {
                        return;
                    }
                    let lands = (now + COMMIT_MARGIN_NS - grid).div_ceil(period);
                    let next = known.map(|(last, p)| (next_vblank_after(now, last, p), p));
                    let dues: Vec<Option<u64>> = pending.iter().map(|(_, due)| Some(*due)).collect();
                    if let Some(index) = due_index(&dues, next) {
                        unseen += index;
                        let rest = pending.split_off(index + 1);
                        shown.push((pending.last().unwrap().0, lands));
                        *pending = rest;
                        *in_flight = Some(lands);
                    }
                };
                for k in 1..140u64 {
                    if in_flight.is_some_and(|landed| landed < k) {
                        in_flight = None;
                    }
                    // Frames arriving before this vblank, each looked at as
                    // it comes; the first, finding no grid, asks the kernel.
                    while arrival(next_frame) < at(k) {
                        let now = arrival(next_frame);
                        pending.push((next_frame, first_due + next_frame * period));
                        next_frame += 1;
                        asked |= known.is_none();
                        decide(now, &mut pending, known, &mut in_flight);
                    }
                    if in_flight == Some(k) {
                        in_flight = None;
                    }
                    // The event asked for, with this vblank's timestamp.
                    if asked {
                        known = Some((at(k), period));
                        asked = false;
                    }
                    decide(at(k) + MS, &mut pending, known, &mut in_flight);
                }
                assert_eq!(unseen, 0, "period {period} phase {phase}");
                assert_eq!(shown[0].0, 0, "period {period} phase {phase}: the first frame is the first shown");
                assert!(
                    at(shown[0].1).abs_diff(first_due) <= period / 2,
                    "period {period} phase {phase}: the first frame at the vblank nearest its time: {shown:?}"
                );
                for pair in shown.windows(2) {
                    assert_eq!(pair[1].0, pair[0].0 + 1, "period {period} phase {phase}: {shown:?}");
                    assert_eq!(pair[1].1, pair[0].1 + 1, "period {period} phase {phase}: one vblank each: {shown:?}");
                }
            }
        }
    }

    #[test]
    fn a_commit_closer_than_the_margin_to_a_vblank_is_held() {
        let last = 1_000 * MS;
        assert_eq!(racing(last + P50 - 3 * MS, last, P50), None, "3 ms before: in time");
        assert_eq!(racing(last + P50 - MS, last, P50), Some(last + P50), "1 ms before: held");
        assert_eq!(racing(last + P50 - 1, last, P50), Some(last + P50));
        assert_eq!(racing(last + P50, last, P50), None, "on it: past");
        assert_eq!(racing(last + P50 + MS, last, P50), None);
    }

    /// Frames as the panel sees them: due `due(n)`, arriving `arrival(n)`;
    /// commits checked right after each vblank's flip event, at each
    /// arrival, and at the end of a hold; the kernel lands a commit on the
    /// first vblank at or after it is made. Returns (frame, vblank index)
    /// shown, and how many went back unseen.
    fn panel(
        period: u64,
        vblanks: u64,
        due: impl Fn(u64) -> u64,
        arrival: impl Fn(u64) -> u64,
        guard: bool,
        snap: bool,
    ) -> (Vec<(u64, u64)>, usize) {
        let grid = 10_000 * MS;
        let at = |k: u64| grid + k * period;
        let mut pending: Vec<u64> = Vec::new();
        let mut shown: Vec<(u64, u64)> = Vec::new();
        let (mut in_flight, mut held_until, mut offset): (Option<u64>, Option<u64>, Option<i64>) = (None, None, None);
        let mut unseen = 0;
        let mut next_frame = 0u64;
        let mut try_commit = |now: u64, pending: &mut Vec<u64>, in_flight: &mut Option<u64>, held_until: &mut Option<u64>, last: u64| {
            if in_flight.is_some() || pending.is_empty() {
                return;
            }
            if snap && offset.is_none() {
                offset = Some(snap_offset(due(pending[0]), last, period));
            }
            if guard && let Some(vblank) = racing(now, last, period) {
                *held_until = Some(vblank + RACE_AFTER_NS);
                return;
            }
            let dues: Vec<Option<u64>> =
                pending.iter().map(|&n| Some(due(n).saturating_add_signed(offset.unwrap_or(0)))).collect();
            if let Some(index) = due_index(&dues, Some((next_vblank_after(now, last, period), period))) {
                unseen += index;
                let frame = pending[index];
                pending.drain(..=index);
                let lands = (now - grid).div_ceil(period);
                shown.push((frame, lands));
                *in_flight = Some(lands);
            }
        };
        for k in 1..vblanks {
            let last = at(k - 1);
            if in_flight == Some(k - 1) {
                in_flight = None;
            }
            try_commit(last + 100_000, &mut pending, &mut in_flight, &mut held_until, last);
            loop {
                let next_arrival = arrival(next_frame);
                let held = held_until.filter(|&t| t < at(k));
                match (next_arrival < at(k), held) {
                    (true, Some(t)) if t < next_arrival => {
                        held_until = None;
                        try_commit(t, &mut pending, &mut in_flight, &mut held_until, last);
                    }
                    (true, _) => {
                        pending.push(next_frame);
                        next_frame += 1;
                        try_commit(next_arrival + 100_000, &mut pending, &mut in_flight, &mut held_until, last);
                    }
                    (false, Some(t)) => {
                        held_until = None;
                        try_commit(t, &mut pending, &mut in_flight, &mut held_until, last);
                    }
                    (false, None) => break,
                }
            }
        }
        (shown, unseen)
    }

    /// The vblank a frame is judged against when it goes back unseen: the
    /// one `due_index` puts it up at.
    #[test]
    fn a_frame_belongs_to_the_vblank_nearest_its_time() {
        let (last, period) = (1000 * MS, 20 * MS);
        assert_eq!(own_vblank(1020 * MS, last, period), 1020 * MS);
        assert_eq!(own_vblank(1029 * MS, last, period), 1020 * MS);
        assert_eq!(own_vblank(1030 * MS, last, period), 1020 * MS);
        assert_eq!(own_vblank(1031 * MS, last, period), 1040 * MS);
        assert_eq!(own_vblank(985 * MS, last, period), 980 * MS);
        for due in (1011..1300).map(|ms| ms * MS) {
            let vblank = own_vblank(due, last, period);
            let next = next_vblank_after(vblank - COMMIT_MARGIN_NS - 1, last, period);
            assert_eq!(due_index(&[Some(due)], Some((next, period))), Some(0), "due {due}");
            assert_eq!(next, vblank, "committed just in time, it lands on its own vblank");
        }
    }

    fn one_vblank_each(shown: &[(u64, u64)]) -> bool {
        shown.windows(2).all(|pair| pair[1].0 == pair[0].0 + 1 && pair[1].1 == pair[0].1 + 1)
    }

    /// The case measured on the tuner (2026-10-31): due times on a fixed
    /// phase, 9.4 ms after a vblank, and the frames arriving 69.7 ms before
    /// them -- 0.3 ms before a vblank -- give or take the wake-up's jitter,
    /// so some arrive just after it. Without the hold, a commit 0.15 ms
    /// before a vblank landed on it a period early and the next one, just
    /// after, where it was meant to: one frame up for two vblanks.
    #[test]
    fn frames_arriving_at_a_vblank_land_where_they_were_meant_to() {
        let grid = 10_000 * MS;
        let due = |n: u64| grid + (n + 13) * P50 + 9_400_000;
        let arrival = |n: u64| due(n) - 69_700_000 + if n % 7 == 3 { 450_000 } else { 0 };
        let (shown, unseen) = panel(P50, 300, due, arrival, true, false);
        assert_eq!(unseen, 0);
        assert!(shown.len() > 250);
        assert!(one_vblank_each(&shown), "{shown:?}");
        let (old, old_unseen) = panel(P50, 300, due, arrival, false, false);
        assert!(!one_vblank_each(&old) || old_unseen > 0, "the measured fault, without the hold");
    }

    /// A film whose due times sit 0.2 ms from the midpoint between vblanks
    /// (measured: 9.5-9.6 ms after one) while the
    /// panel's clock runs 75 ppm off the board's (19.9985 ms against 20 ms,
    /// as measured): unsnapped, a frame is shown twice within the first
    /// seconds; snapped to the vblank, the first five seconds and more are
    /// one frame a vblank.
    #[test]
    fn a_film_snapped_to_the_vblanks_has_minutes_before_the_clocks_part() {
        let grid = 10_000 * MS;
        // The board's 20 ms is the panel's 20.0015: due times drift later.
        let due = |n: u64| grid + 13 * P50 + n * (P50 + 1_500) + 9_800_000;
        let arrival = |n: u64| due(n) - 69_700_000;
        let (unsnapped, _) = panel(P50, 600, due, arrival, true, false);
        assert!(!one_vblank_each(&unsnapped[..300]), "the half-period phase fails within six seconds");
        let (snapped, unseen) = panel(P50, 600, due, arrival, true, true);
        assert_eq!(unseen, 0);
        assert!(snapped.len() > 550);
        assert!(one_vblank_each(&snapped), "{snapped:?}");
    }

    #[test]
    fn the_snap_is_to_the_nearest_vblank_and_never_more_than_half_a_period() {
        let last = 1_000 * MS;
        assert_eq!(snap_offset(last + 5 * P50, last, P50), 0);
        assert_eq!(snap_offset(last + 5 * P50 + 9 * MS, last, P50), -9 * MS as i64);
        assert_eq!(snap_offset(last + 5 * P50 + 11 * MS, last, P50), 9 * MS as i64);
        assert_eq!(snap_offset(last - 3 * MS, last, P50), 3 * MS as i64, "before the last vblank");
        for due in (last..last + 3 * P5994).step_by(1_234_567) {
            let offset = snap_offset(due, last, P5994);
            assert!(offset.unsigned_abs() <= P5994 / 2);
            assert_eq!((due as i64 + offset - last as i64).rem_euclid(P5994 as i64), 0);
        }
    }

    /// An HDR message (88 bytes) and a timed frame (96) queued together on
    /// the stream socket, as the player sends them at a channel's first
    /// frame: read as two messages, the frame's due time with it.
    #[test]
    fn messages_queued_together_on_the_stream_are_read_apart() {
        use std::io::Write;
        let (mut player, interface) = UnixStream::pair().unwrap();
        let mut hdr = [0u8; MESSAGE];
        hdr[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        hdr[4..8].copy_from_slice(&KIND_HDR.to_le_bytes());
        let mut frame = [0u8; TIMED];
        frame[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        frame[4..8].copy_from_slice(&KIND_FRAME_TIMED.to_le_bytes());
        frame[8..16].copy_from_slice(&7u64.to_le_bytes());
        frame[MESSAGE..].copy_from_slice(&123_456_789u64.to_le_bytes());
        player.write_all(&hdr).unwrap();
        player.write_all(&frame).unwrap();
        interface.set_nonblocking(true).unwrap();
        let mut wire = [0u8; TIMED];
        let (read, _) = recv_message(interface.as_raw_fd(), &mut wire).unwrap().unwrap();
        assert_eq!((read, u32::from_le_bytes(wire[4..8].try_into().unwrap())), (MESSAGE, KIND_HDR));
        let (read, _) = recv_message(interface.as_raw_fd(), &mut wire).unwrap().unwrap();
        assert_eq!(read, TIMED);
        assert_eq!(u32::from_le_bytes(wire[0..4].try_into().unwrap()), MAGIC);
        assert_eq!(u64::from_le_bytes(wire[8..16].try_into().unwrap()), 7);
        assert_eq!(u64::from_le_bytes(wire[MESSAGE..].try_into().unwrap()), 123_456_789);
        assert!(recv_message(interface.as_raw_fd(), &mut wire).unwrap().is_none());
    }

    #[test]
    fn sixteen_by_nine_fills_a_sixteen_by_nine_panel() {
        let fitted = fit((1920, 1080), &frame(1920, 1080), PANEL);
        assert_eq!(fitted, PANEL);
    }

    /// A scope film is letterboxed, and the bars are equal.
    #[test]
    fn a_wider_film_is_letterboxed() {
        let fitted = fit((2048, 858), &frame(2048, 858), PANEL);
        assert_eq!(fitted.width, 1920);
        assert_eq!(fitted.height, 804);
        assert_eq!(fitted.y, 138);
        assert_eq!(fitted.y as u32 * 2 + fitted.height, PANEL.height);
    }

    /// Academy ratio, or a phone video: pillarboxed instead.
    #[test]
    fn a_taller_film_is_pillarboxed() {
        let fitted = fit((1440, 1080), &frame(1440, 1080), PANEL);
        assert_eq!(fitted.height, 1080);
        assert_eq!(fitted.width, 1440);
        assert_eq!(fitted.x, 240);
    }

    /// Anamorphic: stored at 1920x1080, shown at 2.39:1. The stored size is the
    /// wrong answer and this is the whole reason the decoder sends both.
    #[test]
    fn the_decoders_aspect_wins_over_the_stored_one() {
        let anamorphic = fit((2560, 1080), &frame(1920, 1080), PANEL);
        assert_eq!(anamorphic.height, 810);
        assert_ne!(anamorphic, fit((1920, 1080), &frame(1920, 1080), PANEL));
    }

    /// A decoder that says nothing about aspect still gets a picture.
    #[test]
    fn without_an_aspect_the_stored_size_is_used() {
        assert_eq!(fit((0, 0), &frame(1920, 1080), PANEL), PANEL);
    }

    /// The message layout is a contract with a C file in another repository's
    /// build. It is written down in exactly two places and this is one of them.
    #[test]
    fn the_wire_is_the_size_both_ends_agree_on() {
        assert_eq!(MESSAGE, 88);
        assert_eq!(REPLY, 16);
        assert_eq!(MAGIC, u32::from_le_bytes(*b"MBV1"));
    }

    fn subtitle_message(x: u32, y: u32, width: u32, height: u32, stride: u32) -> [u8; MESSAGE] {
        let mut bytes = [0u8; MESSAGE];
        bytes[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        bytes[4..8].copy_from_slice(&KIND_SUBTITLE.to_le_bytes());
        for (at, value) in [(8, 1920), (12, 1080), (16, x), (20, y), (24, width), (28, height), (32, stride)] {
            bytes[at..at + 4].copy_from_slice(&u32::to_le_bytes(value));
        }
        bytes
    }

    fn memfd(contents: &[u8]) -> OwnedFd {
        use std::io::Write;
        let fd = unsafe { libc::memfd_create(c"test".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(fd >= 0);
        let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        file.write_all(contents).unwrap();
        OwnedFd::from(file)
    }

    #[test]
    fn a_subtitle_picture_is_read_as_rgba_row_by_row() {
        // Two pixels a row, a stride of three: the third is padding.
        let bgra = [
            1, 2, 3, 4, 5, 6, 7, 8, 0xee, 0xee, 0xee, 0xee, //
            9, 10, 11, 12, 13, 14, 15, 16, 0xee, 0xee, 0xee, 0xee,
        ];
        let image = read_subtitle(&subtitle_message(10, 900, 2, 2, 12), &[memfd(&bgra)])
            .unwrap()
            .unwrap();
        assert_eq!((image.canvas, image.x, image.y, image.width, image.height), ((1920, 1080), 10, 900, 2, 2));
        assert_eq!(image.rgba, [3, 2, 1, 4, 7, 6, 5, 8, 11, 10, 9, 12, 15, 14, 13, 16]);
    }

    #[test]
    fn an_empty_subtitle_message_takes_the_picture_off() {
        assert_eq!(read_subtitle(&subtitle_message(0, 0, 0, 0, 0), &[]), Ok(None));
        assert!(read_subtitle(&subtitle_message(0, 0, 4, 4, 16), &[]).is_err());
        assert!(read_subtitle(&subtitle_message(0, 0, 4, 4, 8), &[memfd(&[0; 64])]).is_err());
    }

    #[test]
    fn a_subtitle_goes_where_it_falls_on_the_picture() {
        let image = SubtitleImage {
            canvas: (1920, 1080),
            x: 480,
            y: 900,
            width: 960,
            height: 108,
            rgba: Vec::new(),
        };
        // A 4K panel, the picture filling it: the canvas scaled twice over.
        let whole = Rect { x: 0, y: 0, width: 3840, height: 2160 };
        let at = place_subtitle(&image, whole, (3840, 2160)).unwrap();
        assert_eq!((at.x, at.y, at.width, at.height), (0.25, 900.0 / 1080.0, 0.5, 0.1));
        // Pillarboxed into the middle half of the panel's width.
        let narrow = Rect { x: 960, y: 0, width: 1920, height: 2160 };
        let at = place_subtitle(&image, narrow, (3840, 2160)).unwrap();
        assert_eq!((at.x, at.width), (0.375, 0.25));
    }
}

/// Every film frame's way to the panel, one line per event, for the one
/// question the five-second `pace` summary cannot answer: which vblank a
/// frame missed and why. Off unless `MEDIABOX_TV_FRAME_TRACE` names a file;
/// then each line is `<microseconds> <event> <a> <b>`:
///
///   A id replaced   a frame arrived (replaced: the waiting one it pushed out, or 0)
///   C ui film       an atomic commit went (ui 1/0, film 1/0)
///   c id 0          that commit carried film frame `id`
///   F               the commit in flight reached its vblank
///   V seq at        ...the kernel's vblank: its sequence number and its own
///                   timestamp, in microseconds
///
/// The time is CLOCK_MONOTONIC, the clock of the kernel's vblank timestamps
/// and of the player's own trace (vo_mediabox, MEDIABOX_VO_TRACE), so a
/// frame's PTS, its commit and the vblank that put it on the panel line up by
/// id. A gap of more than one in `seq` between two film commits is a vblank
/// the film missed.
pub mod trace {
    use std::io::Write;
    use std::sync::Mutex;
    use std::time::Duration;

    struct Trace {
        out: std::io::BufWriter<std::fs::File>,
        lines: u32,
    }

    fn monotonic_us() -> u128 {
        let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: a valid clock id and a timespec on the stack.
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
        now.tv_sec as u128 * 1_000_000 + now.tv_nsec as u128 / 1000
    }

    /// The vblank a page flip event reported.
    pub fn vblank(sequence: u32, at: Duration) {
        event('V', u64::from(sequence), at.as_micros() as u64);
    }

    static TRACE: Mutex<Option<Option<Trace>>> = Mutex::new(None);

    pub fn event(kind: char, a: u64, b: u64) {
        let mut guard = TRACE.lock().unwrap_or_else(|e| e.into_inner());
        let slot = guard.get_or_insert_with(|| {
            let path = std::env::var_os("MEDIABOX_TV_FRAME_TRACE")?;
            let file = std::fs::File::create(path).ok()?;
            Some(Trace { out: std::io::BufWriter::new(file), lines: 0 })
        });
        let Some(trace) = slot.as_mut() else { return };
        let at = monotonic_us();
        let _ = writeln!(trace.out, "{at} {kind} {a} {b}");
        trace.lines += 1;
        if trace.lines % 64 == 0 {
            let _ = trace.out.flush();
        }
    }
}

/// How evenly film frames reach the panel: the gaps between consecutive
/// frames put on the plane, as the interface measured them. The player's own
/// counters cannot see this -- it hands a frame over and considers it shown --
/// and a frame that waits on the interface's own drawing is exactly what a
/// viewer sees as a stutter.
pub mod pace {
    use std::sync::Mutex;
    use std::time::Instant;

    struct Pace {
        last: Option<Instant>,
        frames: u64,
        gaps_us: Vec<u64>,
    }

    static PACE: Mutex<Pace> = Mutex::new(Pace { last: None, frames: 0, gaps_us: Vec::new() });

    pub fn presented() {
        let mut pace = PACE.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if let Some(last) = pace.last.replace(now) {
            let gap = now.duration_since(last).as_micros() as u64;
            // A pause or a seek is not a stutter.
            if gap < 2_000_000 {
                pace.gaps_us.push(gap);
            }
        }
        pace.frames += 1;
    }

    /// Frames, the median gap, the longest gap, and how many gaps were more
    /// than half as long again as the median, since the last call. `None`
    /// while no film is moving.
    pub fn drain() -> Option<(u64, f64, f64, usize)> {
        let mut pace = PACE.lock().unwrap_or_else(|e| e.into_inner());
        let frames = std::mem::take(&mut pace.frames);
        let mut gaps = std::mem::take(&mut pace.gaps_us);
        if gaps.is_empty() {
            return None;
        }
        gaps.sort_unstable();
        let median = gaps[gaps.len() / 2];
        let late = gaps.iter().filter(|gap| **gap * 2 > median * 3).count();
        Some((frames, median as f64 / 1000.0, *gaps.last().unwrap() as f64 / 1000.0, late))
    }
}
