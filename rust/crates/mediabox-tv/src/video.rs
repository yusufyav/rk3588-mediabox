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

/// The two plane properties that decide how a frame is converted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Colour {
    /// 0 = BT.601, 1 = BT.709, 2 = BT.2020. The plane's own enum order.
    pub encoding: u64,
    /// 0 = limited, 1 = full.
    pub range: u64,
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

/// `COLOR_ENCODING` and `COLOR_RANGE` on one plane, by name.
///
/// By name because these are the two DRM properties whose identifiers differ
/// between drivers, and unlike `NAME` and `PLANE_MASK` they are ordinary enums
/// whose names this crate keeps.
fn colour_properties<D: ControlDevice>(
    device: &D,
    plane: control::plane::Handle,
) -> (
    Option<control::property::Handle>,
    Option<control::property::Handle>,
) {
    let Ok(properties) = device.get_properties(plane) else {
        return (None, None);
    };
    let mut encoding = None;
    let mut range = None;
    for handle in properties.as_props_and_values().0.iter().copied() {
        let Ok(info) = device.get_property(handle) else {
            continue;
        };
        match info.name().to_str() {
            Ok("COLOR_ENCODING") => encoding = Some(handle),
            Ok("COLOR_RANGE") => range = Some(handle),
            _ => {}
        }
    }
    (encoding, range)
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
            let (encoding, range) = colour_properties(device, handle);
            found.push(Self {
                plane: handle,
                formats,
                encoding,
                range,
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
        self.apply_colour(device, frame.colour);
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
        if done {
            self.applied.set(Some(colour));
        }
    }

    /// Takes the video off the panel. The interface is drawing underneath it
    /// and does not need to be told.
    pub fn hide<D: ControlDevice>(&self, device: &D, crtc: control::crtc::Handle) {
        self.applied.set(None);
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
// The decoder is a separate process, and it has to be: it is mpv, linked
// against the Rockchip ffmpeg that lives under /opt/rk3588-screenbridge, and
// the interface is a cross-compiled Rust binary that must not be. What the two
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
    },
    /// The player is finished with the plane but is still there.
    Stop,
    /// The player went away.
    Gone,
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

/// One message and whatever descriptors came with it.
///
/// `Ok(None)` means nothing was waiting; `Ok(Some((0, _)))` means the peer has
/// gone.
fn recv_message(fd: RawFd, bytes: &mut [u8; MESSAGE]) -> io::Result<Option<(usize, Vec<OwnedFd>)>> {
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
    Ok(Some((read as usize, fds)))
}

/// The socket the player draws through.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    client: Option<UnixStream>,
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
        })
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
            let mut bytes = [0u8; MESSAGE];
            match recv_message(fd, &mut bytes) {
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
                    if read != MESSAGE || u32_at(&bytes, 0) != MAGIC {
                        eprintln!("mediabox-tv.video ignoring a {read} byte message");
                        continue;
                    }
                    match u32_at(&bytes, 4) {
                        KIND_STOP => received.push(Incoming::Stop),
                        KIND_FRAME => {
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
                                    },
                                },
                                display: (u32_at(&bytes, 80), u32_at(&bytes, 84)),
                                fds,
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

/// The film on the panel: the socket, the plane it settled on, and the frames
/// the display controller has not finished with.
pub struct Sink<D: ControlDevice + Clone> {
    server: Server,
    /// Which candidate the video port actually accepted. Settled once, by
    /// using it — see `VideoPlane::candidates`.
    chosen: Option<usize>,
    shown: VecDeque<(u64, Imported<D>)>,
    on: bool,
    refused: bool,
    frames: u64,
}

impl<D: ControlDevice + Clone> Sink<D> {
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            server: Server::bind(path)?,
            chosen: None,
            shown: VecDeque::new(),
            on: false,
            refused: false,
            frames: 0,
        })
    }

    pub fn poll_fd(&self) -> RawFd {
        self.server.poll_fd()
    }

    /// Whether there is a film on the panel right now.
    pub fn showing(&self) -> bool {
        self.on
    }

    pub fn frames(&self) -> u64 {
        self.frames
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
                Incoming::Stop | Incoming::Gone => self.clear(device, crtc, planes),
                Incoming::Frame {
                    id,
                    frame,
                    display,
                    fds,
                } => {
                    self.present(device, crtc, planes, &frame, display, into, id, &fds);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn present(
        &mut self,
        device: &D,
        crtc: control::crtc::Handle,
        planes: &[VideoPlane],
        frame: &Frame,
        display: (u32, u32),
        into: Rect,
        id: u64,
        fds: &[OwnedFd],
    ) {
        let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(AsFd::as_fd).collect();
        let dst = fit(display, frame, into);

        let order: Vec<usize> = match self.chosen {
            Some(index) => vec![index],
            None => (0..planes.len()).collect(),
        };

        let mut last = String::from("no plane on this video port would take the frame");
        for index in order {
            let plane = &planes[index];
            let imported = match plane.import(device, frame, &borrowed) {
                Ok(imported) => imported,
                Err(error) => {
                    last = error;
                    continue;
                }
            };
            match plane.show(device, crtc, &imported, frame, dst) {
                Ok(()) => {
                    if self.chosen != Some(index) {
                        eprintln!(
                            "mediabox-tv.video settled on {} {:?} {}x{} -> {}x{}+{}+{}",
                            plane.name,
                            frame.fourcc,
                            frame.width,
                            frame.height,
                            dst.width,
                            dst.height,
                            dst.x,
                            dst.y
                        );
                    }
                    self.chosen = Some(index);
                    self.on = true;
                    self.refused = false;
                    self.frames += 1;
                    self.shown.push_back((id, imported));
                    while self.shown.len() > IN_FLIGHT {
                        if let Some((old, _)) = self.shown.pop_front() {
                            self.server.release(old);
                        }
                    }
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

    /// Take the film off the panel and give every buffer back.
    pub fn clear(&mut self, device: &D, crtc: control::crtc::Handle, planes: &[VideoPlane]) {
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
}
