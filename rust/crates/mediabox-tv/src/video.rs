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
        // The Esmart plane advertises NV12 and NV15 as LINEAR, and that is what
        // the decoder produces. Saying so explicitly rather than passing
        // Invalid: ADDFB2 without the modifier flag is a different call.
        Some(DrmModifier::Linear)
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
        let Ok(planes) = device.plane_handles() else { return Vec::new() };
        let Ok(resources) = device.resource_handles() else { return Vec::new() };

        let mut found: Vec<Self> = Vec::new();
        for handle in planes {
            if Some(handle) == primary {
                continue;
            }
            let Ok(info) = device.get_plane(handle) else { continue };
            // Which CRTCs a plane may be used on is a bitmask over the resource
            // list's own order, so the resource list is what decodes it — the
            // same way an encoder's possible_crtcs is read.
            if !resources.filter_crtcs(info.possible_crtcs()).contains(&crtc) {
                continue;
            }
            let formats: Vec<u32> = info.formats().to_vec();
            if !formats.contains(&(DrmFourcc::Nv12 as u32)) {
                continue;
            }
            found.push(Self { plane: handle, formats, name: format!("{handle:?}") });
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
        if dma_bufs.is_empty() || dma_bufs.len() < frame.planes {
            return Err("a frame arrived with fewer file descriptors than planes".into());
        }

        let mut handles: Vec<BufferHandle> = Vec::with_capacity(frame.planes);
        for plane in 0..frame.planes {
            // One descriptor per plane, or one shared by all of them — both are
            // ordinary for a decoder, and the offsets say which.
            let fd = dma_bufs.get(plane).copied().unwrap_or(dma_bufs[0]);
            match device.prime_fd_to_buffer(fd) {
                Ok(handle) => handles.push(handle),
                Err(e) => {
                    for handle in &handles {
                        let _ = device.close_buffer(*handle);
                    }
                    return Err(format!("DRM_IOCTL_PRIME_FD_TO_HANDLE(video plane {plane}): {e}"));
                }
            }
        }

        let mut slots: [Option<BufferHandle>; 4] = [None; 4];
        for (slot, handle) in slots.iter_mut().zip(handles.iter()) {
            *slot = Some(*handle);
        }

        let planar = Planar {
            fourcc: frame.fourcc,
            size: (frame.width, frame.height),
            pitches: frame.pitches,
            handles: slots,
            offsets: frame.offsets,
        };

        match device.add_planar_framebuffer(&planar, control::FbCmd2Flags::MODIFIERS) {
            Ok(framebuffer) => {
                Ok(Imported { device: device.clone(), framebuffer, handles })
            }
            Err(e) => {
                for handle in &handles {
                    let _ = device.close_buffer(*handle);
                }
                Err(format!("DRM_IOCTL_MODE_ADDFB2(video frame): {e}"))
            }
        }
    }

    /// Puts an imported frame on the panel.
    ///
    /// `drmModeSetPlane` rather than an atomic commit: the interface's own
    /// presentation is a legacy page flip on the primary, and mixing the two
    /// ways of driving one CRTC is how a display pipeline stops being
    /// explicable. The scaler is in the call — `src` is the frame, `dst` is
    /// where the interface wants it, and the display controller does the rest.
    pub fn show<D: ControlDevice>(
        &self,
        device: &D,
        crtc: control::crtc::Handle,
        imported: &Imported<D>,
        frame: &Frame,
        dst: Rect,
    ) -> Result<(), String> {
        device
            .set_plane(
                self.plane,
                crtc,
                Some(imported.framebuffer),
                0,
                (dst.x, dst.y, dst.width, dst.height),
                (0, 0, frame.width << 16, frame.height << 16),
            )
            .map_err(|e| format!("DRM_IOCTL_MODE_SETPLANE(video): {e}"))
    }

    /// Takes the video off the panel. The interface is drawing underneath it
    /// and does not need to be told.
    pub fn hide<D: ControlDevice>(&self, device: &D) {
        if let Err(e) = device.set_plane(
            self.plane,
            unsafe { std::mem::transmute::<u32, control::crtc::Handle>(0) },
            None,
            0,
            (0, 0, 0, 0),
            (0, 0, 0, 0),
        ) {
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
