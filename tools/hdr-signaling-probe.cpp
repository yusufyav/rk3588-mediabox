// hdr-signaling-probe - Gate MP1a HDR signalling isolation probe.
//
// Answers exactly one question: when the vendor RK3588 Linux DRM stack is
// handed a correct 10-bit + BT.2020 + HDR10 metadata atomic state, does the
// HDMI sink enter HDR10 mode?
//
// The chain under test is
//
//     HEVC Main10 HDR10 -> RKMPP -> DRM PRIME (NV15) -> atomic KMS -> VOP2 -> HDMI
//
// with no compositor, no GPU user space, no colour conversion and no software
// decode anywhere in it.
//
// The DRM discovery, atomic-commit and DRM PRIME import structure is derived
// from yusufyav/rk3588-screenbridge (tools/hdmirx-direct-display.cpp), reduced
// to what this gate needs and re-targeted from V4L2 capture to file playback.
//
// Steps form an A/B ladder, one variable per rung:
//
//     a0  NV12  Colorspace=Default     color_depth=Automatic  HDR unset
//     a1  NV15  Colorspace=Default     color_depth=Automatic  HDR unset
//     a2  NV15  Colorspace=BT2020_YCC  color_depth=Automatic  HDR unset
//     a3  NV15  Colorspace=BT2020_YCC  color_depth=30bit      HDR unset
//     a4  NV15  Colorspace=BT2020_YCC  color_depth=30bit      HDR_OUTPUT_METADATA
//
// This board has no standard "max bpc" connector property; the vendor exposes
// a "color_depth" enum instead, which is why a3 exists as its own rung.

#include <cerrno>
#include <cinttypes>
#include <cmath>
#include <csignal>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <map>
#include <string>
#include <vector>

#include <deque>

#include <fcntl.h>
#include <poll.h>
#include <sys/stat.h>
#include <unistd.h>

#include <xf86drm.h>
#include <xf86drmMode.h>
#include <drm_fourcc.h>
#include <drm_mode.h>

extern "C" {
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_drm.h>
#include <libavutil/mastering_display_metadata.h>
#include <libavutil/pixdesc.h>
}

namespace {

// ---------------------------------------------------------------- diagnostics

int g_exit_code = 0;

void logf(const char *fmt, ...) {
    va_list args;
    va_start(args, fmt);
    vprintf(fmt, args);
    va_end(args);
    fputc('\n', stdout);
    fflush(stdout);
}

[[noreturn]] void fail(const char *what) {
    fprintf(stderr, "error: %s: %s\n", what, strerror(errno));
    fflush(nullptr);
    _exit(EXIT_FAILURE);
}

void dump_file(const char *path, const char *label) {
    FILE *f = fopen(path, "r");
    if (!f) {
        logf("%s unavailable (%s)", label, strerror(errno));
        return;
    }
    logf("---- %s begin (%s) ----", label, path);
    char line[1024];
    while (fgets(line, sizeof(line), f)) fputs(line, stdout);
    logf("---- %s end ----", label);
    fclose(f);
    fflush(stdout);
}

// ------------------------------------------------------------------ DRM state

struct PropertyInfo {
    uint32_t id = 0;
    uint64_t value = 0;
    bool present = false;
    std::map<std::string, uint64_t> enums;
};

PropertyInfo lookup_property(int fd, uint32_t object, uint32_t type, const char *name) {
    PropertyInfo info;
    drmModeObjectProperties *props = drmModeObjectGetProperties(fd, object, type);
    if (!props) return info;
    for (uint32_t i = 0; i < props->count_props && !info.present; ++i) {
        drmModePropertyRes *p = drmModeGetProperty(fd, props->props[i]);
        if (!p) continue;
        if (!strcmp(p->name, name)) {
            info.id = p->prop_id;
            info.value = props->prop_values[i];
            info.present = true;
            for (int e = 0; e < p->count_enums; ++e)
                info.enums[p->enums[e].name] = p->enums[e].value;
        }
        drmModeFreeProperty(p);
    }
    drmModeFreeObjectProperties(props);
    return info;
}

uint32_t require_property(int fd, uint32_t object, uint32_t type, const char *name) {
    PropertyInfo info = lookup_property(fd, object, type, name);
    if (!info.present) {
        fprintf(stderr, "error: DRM object %u lacks required property \"%s\"\n", object, name);
        _exit(EXIT_FAILURE);
    }
    return info.id;
}

// Reads a property back from the kernel after a commit, so that "requested"
// and "actual" can be reported separately.
bool read_property_value(int fd, uint32_t object, uint32_t type, const char *name, uint64_t *out) {
    PropertyInfo info = lookup_property(fd, object, type, name);
    if (!info.present) return false;
    *out = info.value;
    return true;
}

const char *enum_name_for(const PropertyInfo &info, uint64_t value) {
    for (const auto &entry : info.enums)
        if (entry.second == value) return entry.first.c_str();
    return "<not-an-enum-value>";
}

struct Display {
    int fd = -1;
    bool master = false;

    uint32_t connector_id = 0;
    uint32_t crtc_id = 0;
    int crtc_index = -1;
    uint32_t plane_id = 0;
    uint32_t plane_type = 0;

    drmModeModeInfo mode{};
    uint32_t mode_blob_id = 0;
    uint32_t hdr_blob_id = 0;
    uint32_t last_fb_id = 0;

    // Connector values as found before this run. The kernel fbdev client
    // restores its own mode when we drop master but leaves these alone, so a
    // probe that just exits would strand the link in BT.2020/10-bit/HDR10 while
    // an SDR console is on screen.
    bool saved_connector_state = false;
    uint64_t saved_colorspace = 0;
    uint64_t saved_color_depth = 0;

    uint32_t p_conn_crtc = 0;
    uint32_t p_crtc_active = 0;
    uint32_t p_crtc_mode = 0;
    uint32_t p_plane_fb = 0;
    uint32_t p_plane_crtc = 0;
    uint32_t p_src_x = 0, p_src_y = 0, p_src_w = 0, p_src_h = 0;
    uint32_t p_crtc_x = 0, p_crtc_y = 0, p_crtc_w = 0, p_crtc_h = 0;

    PropertyInfo colorspace;
    PropertyInfo color_depth;
    PropertyInfo hdr_metadata;

    // Planes still bound to our CRTC by the previous master (the kernel fbdev
    // client). They are disabled on modeset so that the only surface VOP2 sees
    // is the one under test; a leftover SDR RGB window would otherwise sit in
    // the same blending path we are trying to characterise.
    struct StalePlane {
        uint32_t id;
        uint32_t fb_prop;
        uint32_t crtc_prop;
    };
    std::vector<StalePlane> stale_planes;
};

Display *g_display = nullptr;

// Set while a run owns imported framebuffers; they reference the DRM fd and so
// must be torn down before it closes, including on the signal path.
void (*g_release_framebuffers)() = nullptr;

// Restores whatever we can before leaving: release framebuffers, destroy our
// blobs and drop DRM master so the kernel fbdev client takes the CRTC back.
// Safe to call twice.
// Puts the connector's colour properties back the way they were found. Runs
// before the framebuffers go away because the commit still needs one.
void restore_connector_state(Display &d) {
    if (!d.saved_connector_state || !d.master || !d.last_fb_id || !d.mode_blob_id) return;
    drmModeAtomicReq *req = drmModeAtomicAlloc();
    if (!req) return;
    drmModeAtomicAddProperty(req, d.connector_id, d.p_conn_crtc, d.crtc_id);
    drmModeAtomicAddProperty(req, d.crtc_id, d.p_crtc_active, 1);
    drmModeAtomicAddProperty(req, d.crtc_id, d.p_crtc_mode, d.mode_blob_id);
    if (d.colorspace.present)
        drmModeAtomicAddProperty(req, d.connector_id, d.colorspace.id, d.saved_colorspace);
    if (d.color_depth.present)
        drmModeAtomicAddProperty(req, d.connector_id, d.color_depth.id, d.saved_color_depth);
    if (d.hdr_metadata.present)
        drmModeAtomicAddProperty(req, d.connector_id, d.hdr_metadata.id, 0);
    drmModeAtomicAddProperty(req, d.plane_id, d.p_plane_fb, d.last_fb_id);
    drmModeAtomicAddProperty(req, d.plane_id, d.p_plane_crtc, d.crtc_id);
    drmModeAtomicAddProperty(req, d.plane_id, d.p_src_x, 0);
    drmModeAtomicAddProperty(req, d.plane_id, d.p_src_y, 0);
    drmModeAtomicAddProperty(req, d.plane_id, d.p_src_w,
                             static_cast<uint64_t>(d.mode.hdisplay) << 16);
    drmModeAtomicAddProperty(req, d.plane_id, d.p_src_h,
                             static_cast<uint64_t>(d.mode.vdisplay) << 16);
    drmModeAtomicAddProperty(req, d.plane_id, d.p_crtc_x, 0);
    drmModeAtomicAddProperty(req, d.plane_id, d.p_crtc_y, 0);
    drmModeAtomicAddProperty(req, d.plane_id, d.p_crtc_w, d.mode.hdisplay);
    drmModeAtomicAddProperty(req, d.plane_id, d.p_crtc_h, d.mode.vdisplay);
    const int ret = drmModeAtomicCommit(d.fd, req, DRM_MODE_ATOMIC_ALLOW_MODESET, nullptr);
    drmModeAtomicFree(req);
    if (ret) fprintf(stderr, "warning: connector state restore failed: %s\n", strerror(-ret));
}

void restore_display() {
    if (!g_display || g_display->fd < 0) return;
    Display &d = *g_display;
    restore_connector_state(d);
    if (g_release_framebuffers) {
        g_release_framebuffers();
        g_release_framebuffers = nullptr;
    }
    if (d.hdr_blob_id) {
        drmModeDestroyPropertyBlob(d.fd, d.hdr_blob_id);
        d.hdr_blob_id = 0;
    }
    if (d.mode_blob_id) {
        drmModeDestroyPropertyBlob(d.fd, d.mode_blob_id);
        d.mode_blob_id = 0;
    }
    if (d.master) {
        drmDropMaster(d.fd);
        d.master = false;
    }
    close(d.fd);
    d.fd = -1;
}

void on_signal(int sig) {
    // Async-signal-safe enough for a diagnostic tool: restore and leave.
    restore_display();
    _exit(128 + sig);
}

void install_signal_handlers() {
    struct sigaction sa {};
    sa.sa_handler = on_signal;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGINT, &sa, nullptr);
    sigaction(SIGTERM, &sa, nullptr);
    sigaction(SIGHUP, &sa, nullptr);
    sigaction(SIGALRM, &sa, nullptr);
    // A probe that wedges while holding DRM master leaves the console with no
    // display, so an unconditional watchdog arms alongside the handlers and is
    // re-armed with the real budget once the hold length is known.
    signal(SIGPIPE, SIG_IGN);
}

// ------------------------------------------------------------------ inventory

void fourcc_string(uint32_t format, char out[5]) {
    out[0] = static_cast<char>(format & 0xff);
    out[1] = static_cast<char>((format >> 8) & 0xff);
    out[2] = static_cast<char>((format >> 16) & 0xff);
    out[3] = static_cast<char>((format >> 24) & 0xff);
    out[4] = '\0';
}

bool plane_has_format(const drmModePlane *plane, uint32_t format) {
    for (uint32_t i = 0; i < plane->count_formats; ++i)
        if (plane->formats[i] == format) return true;
    return false;
}

double mode_refresh(const drmModeModeInfo &m) {
    if (!m.htotal || !m.vtotal) return 0.0;
    double refresh = static_cast<double>(m.clock) * 1000.0 /
                     (static_cast<double>(m.htotal) * static_cast<double>(m.vtotal));
    if (m.flags & DRM_MODE_FLAG_INTERLACE) refresh *= 2.0;
    return refresh;
}

int open_card(const char *path) {
    int fd = open(path, O_RDWR | O_CLOEXEC);
    if (fd < 0) fail("open DRM card");
    if (drmSetClientCap(fd, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1))
        fail("drmSetClientCap UNIVERSAL_PLANES");
    if (drmSetClientCap(fd, DRM_CLIENT_CAP_ATOMIC, 1)) fail("drmSetClientCap ATOMIC");
    return fd;
}

int run_probe(const char *card) {
    int fd = open_card(card);
    drmModeRes *res = drmModeGetResources(fd);
    if (!res) fail("drmModeGetResources");

    logf("probe card=%s crtcs=%d connectors=%d", card, res->count_crtcs, res->count_connectors);

    for (int i = 0; i < res->count_connectors; ++i) {
        drmModeConnector *c = drmModeGetConnector(fd, res->connectors[i]);
        if (!c) continue;
        if (c->connector_type != DRM_MODE_CONNECTOR_HDMIA || c->connection != DRM_MODE_CONNECTED) {
            drmModeFreeConnector(c);
            continue;
        }
        logf("connector id=%u HDMI-A-%u status=connected modes=%d", c->connector_id,
             c->connector_type_id, c->count_modes);
        for (int m = 0; m < c->count_modes; ++m) {
            const drmModeModeInfo &mm = c->modes[m];
            if (mm.hdisplay != 3840) continue;
            logf("  mode[%02d] %s %ux%u clock=%u refresh=%.3f flags=0x%x", m, mm.name, mm.hdisplay,
                 mm.vdisplay, mm.clock, mode_refresh(mm), mm.flags);
        }
        static const char *kProps[] = {"Colorspace",  "color_depth",     "color_depth_caps",
                                       "color_format", "color_format_caps", "HDR_OUTPUT_METADATA",
                                       "max bpc",     "quant_range",     "HDR_PANEL_METADATA"};
        for (const char *name : kProps) {
            PropertyInfo info = lookup_property(fd, c->connector_id, DRM_MODE_OBJECT_CONNECTOR, name);
            if (!info.present) {
                logf("  property %-22s ABSENT", name);
                continue;
            }
            std::string enums;
            for (const auto &e : info.enums) {
                enums += " ";
                enums += e.first;
                enums += "=";
                enums += std::to_string(e.second);
            }
            logf("  property %-22s id=%u value=%" PRIu64 "%s", name, info.id, info.value,
                 enums.empty() ? "" : (" enums:" + enums).c_str());
        }
        drmModeFreeConnector(c);
    }

    drmModePlaneRes *planes = drmModeGetPlaneResources(fd);
    if (planes) {
        for (uint32_t i = 0; i < planes->count_planes; ++i) {
            drmModePlane *p = drmModeGetPlane(fd, planes->planes[i]);
            if (!p) continue;
            PropertyInfo type = lookup_property(fd, p->plane_id, DRM_MODE_OBJECT_PLANE, "type");
            const bool nv12 = plane_has_format(p, DRM_FORMAT_NV12);
            const bool nv15 = plane_has_format(p, DRM_FORMAT_NV15);
            if (nv12 || nv15)
                logf("plane id=%u type=%s possible_crtcs=0x%x nv12=%d nv15=%d", p->plane_id,
                     type.present ? enum_name_for(type, type.value) : "?", p->possible_crtcs, nv12,
                     nv15);
            drmModeFreePlane(p);
        }
        drmModeFreePlaneResources(planes);
    }

    drmModeFreeResources(res);
    close(fd);
    logf("probe complete; no modeset performed");
    return 0;
}

// -------------------------------------------------------------- mode + plane

// Picks the 3840x2160 mode whose refresh is closest to the requested rate.
bool select_mode(drmModeConnector *conn, double target_hz, drmModeModeInfo *out, int *out_index) {
    double best_error = 1e9;
    int best = -1;
    for (int m = 0; m < conn->count_modes; ++m) {
        const drmModeModeInfo &mm = conn->modes[m];
        if (mm.hdisplay != 3840 || mm.vdisplay != 2160) continue;
        if (mm.flags & DRM_MODE_FLAG_INTERLACE) continue;
        const double error = std::fabs(mode_refresh(mm) - target_hz);
        if (error < best_error) {
            best_error = error;
            best = m;
        }
    }
    if (best < 0) return false;
    *out = conn->modes[best];
    *out_index = best;
    return true;
}

// ------------------------------------------------------------- HDR metadata

struct HdrSource {
    bool have_mastering = false;
    bool have_light_level = false;
    AVMasteringDisplayMetadata mastering{};
    AVContentLightMetadata light{};
    enum AVColorTransferCharacteristic trc = AVCOL_TRC_UNSPECIFIED;
    enum AVColorPrimaries primaries = AVCOL_PRI_UNSPECIFIED;
    enum AVColorSpace space = AVCOL_SPC_UNSPECIFIED;
};

uint16_t to_primary_units(AVRational value) {
    // CTA-861.3 codes chromaticity in units of 0.00002.
    const double v = av_q2d(value) * 50000.0;
    if (v < 0.0) return 0;
    if (v > 65535.0) return 65535;
    return static_cast<uint16_t>(std::lround(v));
}

uint16_t to_max_luminance_units(AVRational value) {
    const double v = av_q2d(value);  // already cd/m2, coded in units of 1 cd/m2
    if (v < 0.0) return 0;
    if (v > 65535.0) return 65535;
    return static_cast<uint16_t>(std::lround(v));
}

uint16_t to_min_luminance_units(AVRational value) {
    const double v = av_q2d(value) * 10000.0;  // units of 0.0001 cd/m2
    if (v < 0.0) return 0;
    if (v > 65535.0) return 65535;
    return static_cast<uint16_t>(std::lround(v));
}

// Builds the CTA-861.3 Static Metadata Descriptor Type 1 payload from what the
// asset actually carries. Absent fields are left zero, which is the encoding
// for "unspecified"; nothing is invented.
bool build_hdr_metadata(const HdrSource &src, struct hdr_output_metadata *out, std::string *notes) {
    memset(out, 0, sizeof(*out));

    if (src.trc != AVCOL_TRC_SMPTE2084) {
        *notes = "stream transfer characteristic is not SMPTE ST2084; refusing to signal HDR10";
        return false;
    }

    out->metadata_type = 0;  // HDMI_STATIC_METADATA_TYPE1
    out->hdmi_metadata_type1.metadata_type = 0;
    out->hdmi_metadata_type1.eotf = 2;  // HDMI_EOTF_SMPTE_ST2084

    if (src.have_mastering && src.mastering.has_primaries) {
        // FFmpeg normalises ST 2086 primaries to R, G, B order. CTA-861.3
        // carries them in ST 2086 order, which is G, B, R.
        static const int kFfmpegIndexForInfoframeSlot[3] = {1, 2, 0};
        for (int slot = 0; slot < 3; ++slot) {
            const int i = kFfmpegIndexForInfoframeSlot[slot];
            out->hdmi_metadata_type1.display_primaries[slot].x =
                to_primary_units(src.mastering.display_primaries[i][0]);
            out->hdmi_metadata_type1.display_primaries[slot].y =
                to_primary_units(src.mastering.display_primaries[i][1]);
        }
        out->hdmi_metadata_type1.white_point.x = to_primary_units(src.mastering.white_point[0]);
        out->hdmi_metadata_type1.white_point.y = to_primary_units(src.mastering.white_point[1]);
    } else {
        *notes += "mastering display primaries unavailable (left zero); ";
    }

    if (src.have_mastering && src.mastering.has_luminance) {
        out->hdmi_metadata_type1.max_display_mastering_luminance =
            to_max_luminance_units(src.mastering.max_luminance);
        out->hdmi_metadata_type1.min_display_mastering_luminance =
            to_min_luminance_units(src.mastering.min_luminance);
    } else {
        *notes += "mastering display luminance unavailable (left zero); ";
    }

    if (src.have_light_level) {
        out->hdmi_metadata_type1.max_cll = static_cast<uint16_t>(
            src.light.MaxCLL > 65535 ? 65535 : src.light.MaxCLL);
        out->hdmi_metadata_type1.max_fall = static_cast<uint16_t>(
            src.light.MaxFALL > 65535 ? 65535 : src.light.MaxFALL);
    } else {
        *notes += "MaxCLL/MaxFALL unavailable (left zero, which encodes unknown); ";
    }
    return true;
}

void log_hdr_metadata(const struct hdr_output_metadata &m) {
    const struct hdr_metadata_infoframe &f = m.hdmi_metadata_type1;
    logf("hdr_metadata blob_size=%zu metadata_type=%u eotf=%u infoframe_metadata_type=%u",
         sizeof(m), m.metadata_type, f.eotf, f.metadata_type);
    logf("hdr_metadata display_primaries(ST2086 order G,B,R)=[(%u,%u) (%u,%u) (%u,%u)] "
         "white_point=(%u,%u)",
         f.display_primaries[0].x, f.display_primaries[0].y, f.display_primaries[1].x,
         f.display_primaries[1].y, f.display_primaries[2].x, f.display_primaries[2].y,
         f.white_point.x, f.white_point.y);
    logf("hdr_metadata max_display_mastering_luminance=%u (cd/m2) "
         "min_display_mastering_luminance=%u (0.0001 cd/m2) max_cll=%u max_fall=%u",
         f.max_display_mastering_luminance, f.min_display_mastering_luminance, f.max_cll,
         f.max_fall);
    const uint8_t *raw = reinterpret_cast<const uint8_t *>(&m);
    std::string hex;
    char byte[4];
    for (size_t i = 0; i < sizeof(m); ++i) {
        snprintf(byte, sizeof(byte), "%02x", raw[i]);
        hex += byte;
        if (i + 1 < sizeof(m)) hex += " ";
    }
    logf("hdr_metadata raw=%s", hex.c_str());
}

// --------------------------------------------------------------- decode side

enum AVPixelFormat pick_drm_prime(AVCodecContext *ctx, const enum AVPixelFormat *formats) {
    (void)ctx;
    for (const enum AVPixelFormat *p = formats; *p != AV_PIX_FMT_NONE; ++p)
        if (*p == AV_PIX_FMT_DRM_PRIME) return AV_PIX_FMT_DRM_PRIME;
    return formats[0];
}

struct Decoder {
    AVFormatContext *fmt = nullptr;
    AVCodecContext *codec = nullptr;
    int stream_index = -1;
    AVBufferRef *hw_device = nullptr;
};

void decoder_close(Decoder *d) {
    if (d->codec) avcodec_free_context(&d->codec);
    if (d->fmt) avformat_close_input(&d->fmt);
    if (d->hw_device) av_buffer_unref(&d->hw_device);
}

bool decoder_open(Decoder *d, const char *path, const char *decoder_name, std::string *error) {
    *d = Decoder{};
    if (avformat_open_input(&d->fmt, path, nullptr, nullptr) < 0) {
        *error = "avformat_open_input failed";
        return false;
    }
    if (avformat_find_stream_info(d->fmt, nullptr) < 0) {
        *error = "avformat_find_stream_info failed";
        return false;
    }
    d->stream_index = av_find_best_stream(d->fmt, AVMEDIA_TYPE_VIDEO, -1, -1, nullptr, 0);
    if (d->stream_index < 0) {
        *error = "no video stream";
        return false;
    }
    const AVCodec *codec = avcodec_find_decoder_by_name(decoder_name);
    if (!codec) {
        *error = std::string("decoder not built in: ") + decoder_name;
        return false;
    }
    d->codec = avcodec_alloc_context3(codec);
    if (!d->codec) {
        *error = "avcodec_alloc_context3 failed";
        return false;
    }
    if (avcodec_parameters_to_context(d->codec, d->fmt->streams[d->stream_index]->codecpar) < 0) {
        *error = "avcodec_parameters_to_context failed";
        return false;
    }
    d->codec->get_format = pick_drm_prime;

    // The RKMPP decoder needs a hardware device to hand out DRM PRIME frames.
    if (av_hwdevice_ctx_create(&d->hw_device, AV_HWDEVICE_TYPE_RKMPP, nullptr, nullptr, 0) >= 0) {
        d->codec->hw_device_ctx = av_buffer_ref(d->hw_device);
    } else if (av_hwdevice_ctx_create(&d->hw_device, AV_HWDEVICE_TYPE_DRM, "/dev/dri/card0",
                                      nullptr, 0) >= 0) {
        d->codec->hw_device_ctx = av_buffer_ref(d->hw_device);
    }

    if (avcodec_open2(d->codec, codec, nullptr) < 0) {
        *error = "avcodec_open2 failed";
        return false;
    }
    return true;
}

// ------------------------------------------------------------ framebuffers

struct ImportedFrame {
    uint32_t fb_id = 0;
    uint32_t handles[4] = {0, 0, 0, 0};
};

class FramebufferCache {
  public:
    explicit FramebufferCache(int drm_fd) : drm_fd_(drm_fd) {}

    FramebufferCache(const FramebufferCache &) = delete;
    FramebufferCache &operator=(const FramebufferCache &) = delete;

    ~FramebufferCache() { clear(); }

    // Must run while the DRM fd is still open, so restore_display() calls it.
    void clear() {
        for (AVFrame *held : in_flight_) {
            AVFrame *f = held;
            av_frame_free(&f);
        }
        in_flight_.clear();
        for (auto &entry : cache_) {
            if (entry.second.fb_id) drmModeRmFB(drm_fd_, entry.second.fb_id);
            for (uint32_t handle : entry.second.handles) {
                if (!handle) continue;
                struct drm_gem_close close_arg {};
                close_arg.handle = handle;
                drmIoctl(drm_fd_, DRM_IOCTL_GEM_CLOSE, &close_arg);
            }
        }
        cache_.clear();
    }

    // MPP recycles a bounded buffer group, so keying on the dma-buf fd is
    // stable as long as we hold a reference to a frame that owns it.
    bool get(const AVDRMFrameDescriptor *desc, AVFrame *frame, uint32_t *fb_id,
             std::string *error) {
        if (desc->nb_layers != 1) {
            *error = "unexpected DRM PRIME descriptor with " + std::to_string(desc->nb_layers) +
                     " layers";
            return false;
        }
        // Key on the dma-buf inode rather than the fd number: MPP recycles a
        // bounded buffer group and fd numbers can be reused, while the inode
        // identifies the underlying buffer for as long as it exists.
        struct stat st {};
        if (fstat(desc->objects[0].fd, &st)) {
            *error = std::string("fstat on dma-buf fd: ") + strerror(errno);
            return false;
        }
        const uint64_t key = st.st_ino;
        auto it = cache_.find(key);
        if (it != cache_.end()) {
            *fb_id = it->second.fb_id;
            return true;
        }

        ImportedFrame imported;
        uint32_t handles[4] = {0, 0, 0, 0};
        uint32_t pitches[4] = {0, 0, 0, 0};
        uint32_t offsets[4] = {0, 0, 0, 0};
        uint64_t modifiers[4] = {0, 0, 0, 0};

        const AVDRMLayerDescriptor &layer = desc->layers[0];
        for (int i = 0; i < layer.nb_planes; ++i) {
            const AVDRMPlaneDescriptor &plane = layer.planes[i];
            const AVDRMObjectDescriptor &object = desc->objects[plane.object_index];
            uint32_t handle = 0;
            if (drmPrimeFDToHandle(drm_fd_, object.fd, &handle)) {
                *error = std::string("drmPrimeFDToHandle: ") + strerror(errno);
                return false;
            }
            handles[i] = handle;
            imported.handles[i] = handle;
            pitches[i] = static_cast<uint32_t>(plane.pitch);
            offsets[i] = static_cast<uint32_t>(plane.offset);
            modifiers[i] = object.format_modifier;
        }

        const uint32_t flags =
            (modifiers[0] != DRM_FORMAT_MOD_LINEAR && modifiers[0] != DRM_FORMAT_MOD_INVALID)
                ? DRM_MODE_FB_MODIFIERS
                : 0;
        if (drmModeAddFB2WithModifiers(drm_fd_, frame->width, frame->height, layer.format, handles,
                                       pitches, offsets, modifiers, &imported.fb_id, flags)) {
            char fmt[5];
            fourcc_string(layer.format, fmt);
            *error = std::string("drmModeAddFB2WithModifiers format=") + fmt + " modifier=0x" +
                     std::to_string(modifiers[0]) + ": " + strerror(errno);
            return false;
        }
        if (cache_.empty()) {
            char fmt[5];
            fourcc_string(layer.format, fmt);
            logf("framebuffer format=%s modifier=0x%" PRIx64 " planes=%d pitch0=%u offset1=%u "
                 "size=%dx%d",
                 fmt, modifiers[0], layer.nb_planes, pitches[0], offsets[1], frame->width,
                 frame->height);
        }
        *fb_id = imported.fb_id;
        cache_.emplace(key, imported);
        return true;
    }

    // The GEM handle keeps the buffer alive, but MPP may hand the same buffer
    // back to the decoder as soon as the last AVFrame reference drops. Holding
    // the few most recent frames keeps the one on screen out of that rotation
    // without pinning the whole pool, which would starve the decoder.
    void retain_in_flight(AVFrame *frame) {
        AVFrame *held = av_frame_clone(frame);
        if (!held) return;
        in_flight_.push_back(held);
        while (in_flight_.size() > 3) {
            AVFrame *oldest = in_flight_.front();
            in_flight_.pop_front();
            av_frame_free(&oldest);
        }
    }

    size_t size() const { return cache_.size(); }

  private:
    int drm_fd_;
    std::map<uint64_t, ImportedFrame> cache_;
    std::deque<AVFrame *> in_flight_;
};

// ------------------------------------------------------------------- the run

struct StepConfig {
    const char *name;
    uint32_t format;          // DRM fourcc the plane must accept
    bool set_bt2020;
    bool set_30bit;
    bool set_hdr_metadata;
};

const StepConfig *step_for(const char *name) {
    static const StepConfig kSteps[] = {
        {"a0", DRM_FORMAT_NV12, false, false, false},
        {"a1", DRM_FORMAT_NV15, false, false, false},
        {"a2", DRM_FORMAT_NV15, true, false, false},
        {"a3", DRM_FORMAT_NV15, true, true, false},
        {"a4", DRM_FORMAT_NV15, true, true, true},
    };
    for (const StepConfig &s : kSteps)
        if (!strcmp(s.name, name)) return &s;
    return nullptr;
}

struct FlipState {
    bool pending = false;
};

void page_flip_handler(int, unsigned int, unsigned int, unsigned int, void *user) {
    static_cast<FlipState *>(user)->pending = false;
}

void add_prop(drmModeAtomicReq *req, uint32_t object, uint32_t property, uint64_t value) {
    if (drmModeAtomicAddProperty(req, object, property, value) < 0)
        fail("drmModeAtomicAddProperty");
}

// Builds the full atomic request for this step. Split out so the plane search
// can run it as a TEST_ONLY commit before anything is actually programmed.
void build_request(drmModeAtomicReq *req, Display &d, const StepConfig &step, uint32_t fb_id,
                   bool modeset) {
    if (modeset) {
        add_prop(req, d.connector_id, d.p_conn_crtc, d.crtc_id);
        add_prop(req, d.crtc_id, d.p_crtc_active, 1);
        add_prop(req, d.crtc_id, d.p_crtc_mode, d.mode_blob_id);
        for (const Display::StalePlane &stale : d.stale_planes) {
            if (stale.id == d.plane_id) continue;
            add_prop(req, stale.id, stale.fb_prop, 0);
            add_prop(req, stale.id, stale.crtc_prop, 0);
        }
    }
    if (d.colorspace.present) {
        const char *want = step.set_bt2020 ? "BT2020_YCC" : "Default";
        auto it = d.colorspace.enums.find(want);
        if (it != d.colorspace.enums.end())
            add_prop(req, d.connector_id, d.colorspace.id, it->second);
    }
    if (d.color_depth.present) {
        const char *want = step.set_30bit ? "30bit" : "Automatic";
        auto it = d.color_depth.enums.find(want);
        if (it != d.color_depth.enums.end())
            add_prop(req, d.connector_id, d.color_depth.id, it->second);
    }
    if (d.hdr_metadata.present)
        add_prop(req, d.connector_id, d.hdr_metadata.id, step.set_hdr_metadata ? d.hdr_blob_id : 0);

    add_prop(req, d.plane_id, d.p_plane_fb, fb_id);
    add_prop(req, d.plane_id, d.p_plane_crtc, d.crtc_id);
    add_prop(req, d.plane_id, d.p_src_x, 0);
    add_prop(req, d.plane_id, d.p_src_y, 0);
    add_prop(req, d.plane_id, d.p_src_w, static_cast<uint64_t>(d.mode.hdisplay) << 16);
    add_prop(req, d.plane_id, d.p_src_h, static_cast<uint64_t>(d.mode.vdisplay) << 16);
    add_prop(req, d.plane_id, d.p_crtc_x, 0);
    add_prop(req, d.plane_id, d.p_crtc_y, 0);
    add_prop(req, d.plane_id, d.p_crtc_w, d.mode.hdisplay);
    add_prop(req, d.plane_id, d.p_crtc_h, d.mode.vdisplay);
}

void bind_plane_properties(Display &d) {
    d.p_plane_fb = require_property(d.fd, d.plane_id, DRM_MODE_OBJECT_PLANE, "FB_ID");
    d.p_plane_crtc = require_property(d.fd, d.plane_id, DRM_MODE_OBJECT_PLANE, "CRTC_ID");
    d.p_src_x = require_property(d.fd, d.plane_id, DRM_MODE_OBJECT_PLANE, "SRC_X");
    d.p_src_y = require_property(d.fd, d.plane_id, DRM_MODE_OBJECT_PLANE, "SRC_Y");
    d.p_src_w = require_property(d.fd, d.plane_id, DRM_MODE_OBJECT_PLANE, "SRC_W");
    d.p_src_h = require_property(d.fd, d.plane_id, DRM_MODE_OBJECT_PLANE, "SRC_H");
    d.p_crtc_x = require_property(d.fd, d.plane_id, DRM_MODE_OBJECT_PLANE, "CRTC_X");
    d.p_crtc_y = require_property(d.fd, d.plane_id, DRM_MODE_OBJECT_PLANE, "CRTC_Y");
    d.p_crtc_w = require_property(d.fd, d.plane_id, DRM_MODE_OBJECT_PLANE, "CRTC_W");
    d.p_crtc_h = require_property(d.fd, d.plane_id, DRM_MODE_OBJECT_PLANE, "CRTC_H");
}

void report_readback(Display &d, const StepConfig &step) {
    logf("---- requested vs actual ----");
    uint64_t value = 0;
    if (d.colorspace.present) {
        const char *want = step.set_bt2020 ? "BT2020_YCC" : "Default";
        read_property_value(d.fd, d.connector_id, DRM_MODE_OBJECT_CONNECTOR, "Colorspace", &value);
        PropertyInfo now =
            lookup_property(d.fd, d.connector_id, DRM_MODE_OBJECT_CONNECTOR, "Colorspace");
        logf("Colorspace          requested=%s actual=%s (%" PRIu64 ")", want,
             enum_name_for(now, value), value);
    }
    if (d.color_depth.present) {
        const char *want = step.set_30bit ? "30bit" : "Automatic";
        read_property_value(d.fd, d.connector_id, DRM_MODE_OBJECT_CONNECTOR, "color_depth", &value);
        PropertyInfo now =
            lookup_property(d.fd, d.connector_id, DRM_MODE_OBJECT_CONNECTOR, "color_depth");
        logf("color_depth         requested=%s actual=%s (%" PRIu64 ")", want,
             enum_name_for(now, value), value);
    }
    if (read_property_value(d.fd, d.connector_id, DRM_MODE_OBJECT_CONNECTOR, "color_format",
                            &value)) {
        PropertyInfo now =
            lookup_property(d.fd, d.connector_id, DRM_MODE_OBJECT_CONNECTOR, "color_format");
        logf("color_format        requested=<not set by probe> actual=%s (%" PRIu64 ")",
             enum_name_for(now, value), value);
    }
    if (d.hdr_metadata.present) {
        read_property_value(d.fd, d.connector_id, DRM_MODE_OBJECT_CONNECTOR,
                            "HDR_OUTPUT_METADATA", &value);
        logf("HDR_OUTPUT_METADATA requested_blob=%u actual_blob=%" PRIu64,
             step.set_hdr_metadata ? d.hdr_blob_id : 0, value);
    }
}

struct Options {
    const char *card = "/dev/dri/card0";
    const char *asset = nullptr;
    const char *step = nullptr;
    const char *decoder = "hevc_rkmpp";
    double target_hz = 24000.0 / 1001.0;
    int hold_seconds = 20;
    uint32_t forced_plane = 0;
    bool probe_only = false;
    bool reset_only = false;
};

int run_step(const Options &opt) {
    const StepConfig *step = step_for(opt.step);
    if (!step) {
        fprintf(stderr, "error: unknown step \"%s\" (expected a0..a4)\n", opt.step);
        return 2;
    }
    if (!opt.asset) {
        fprintf(stderr, "error: --step requires --asset\n");
        return 2;
    }
    char want_fmt[5];
    fourcc_string(step->format, want_fmt);
    logf("step=%s required_plane_format=%s asset=%s decoder=%s hold=%ds", step->name, want_fmt,
         opt.asset, opt.decoder, opt.hold_seconds);

    // ---- asset metadata, read before touching the display -------------------
    Decoder dec;
    std::string error;
    if (!decoder_open(&dec, opt.asset, opt.decoder, &error)) {
        logf("STEP %s RESULT: BLOCKED (%s)", step->name, error.c_str());
        decoder_close(&dec);
        return 1;
    }
    AVStream *stream = dec.fmt->streams[dec.stream_index];
    logf("asset codec=%s profile=%d %dx%d pix_fmt=%s primaries=%s trc=%s space=%s fps=%d/%d",
         avcodec_get_name(dec.codec->codec_id), dec.codec->profile, dec.codec->width,
         dec.codec->height, av_get_pix_fmt_name(dec.codec->pix_fmt),
         av_color_primaries_name(dec.codec->color_primaries),
         av_color_transfer_name(dec.codec->color_trc),
         av_color_space_name(dec.codec->colorspace), stream->avg_frame_rate.num,
         stream->avg_frame_rate.den);

    HdrSource hdr_source;
    hdr_source.trc = dec.codec->color_trc;
    hdr_source.primaries = dec.codec->color_primaries;
    hdr_source.space = dec.codec->colorspace;
    for (int i = 0; i < stream->codecpar->nb_coded_side_data; ++i) {
        const AVPacketSideData &sd = stream->codecpar->coded_side_data[i];
        if (sd.type == AV_PKT_DATA_MASTERING_DISPLAY_METADATA &&
            sd.size >= sizeof(AVMasteringDisplayMetadata)) {
            hdr_source.mastering = *reinterpret_cast<AVMasteringDisplayMetadata *>(sd.data);
            hdr_source.have_mastering = true;
        } else if (sd.type == AV_PKT_DATA_CONTENT_LIGHT_LEVEL &&
                   sd.size >= sizeof(AVContentLightMetadata)) {
            hdr_source.light = *reinterpret_cast<AVContentLightMetadata *>(sd.data);
            hdr_source.have_light_level = true;
        }
    }

    // ---- display setup ------------------------------------------------------
    Display display;
    g_display = &display;
    display.fd = open_card(opt.card);
    if (drmSetMaster(display.fd)) {
        logf("STEP %s RESULT: BLOCKED (drmSetMaster: %s)", step->name, strerror(errno));
        restore_display();
        decoder_close(&dec);
        return 1;
    }
    display.master = true;

    drmModeRes *res = drmModeGetResources(display.fd);
    if (!res) fail("drmModeGetResources");
    drmModeConnector *conn = nullptr;
    for (int i = 0; i < res->count_connectors && !conn; ++i) {
        drmModeConnector *c = drmModeGetConnector(display.fd, res->connectors[i]);
        if (c && c->connector_type == DRM_MODE_CONNECTOR_HDMIA &&
            c->connection == DRM_MODE_CONNECTED && c->count_modes > 0) {
            conn = c;
            break;
        }
        if (c) drmModeFreeConnector(c);
    }
    if (!conn) {
        logf("STEP %s RESULT: BLOCKED (no connected HDMI-A connector)", step->name);
        drmModeFreeResources(res);
        restore_display();
        decoder_close(&dec);
        return 1;
    }
    display.connector_id = conn->connector_id;

    int mode_index = -1;
    if (!select_mode(conn, opt.target_hz, &display.mode, &mode_index)) {
        logf("STEP %s RESULT: BLOCKED (no progressive 3840x2160 mode on connector %u)", step->name,
             display.connector_id);
        drmModeFreeConnector(conn);
        drmModeFreeResources(res);
        restore_display();
        decoder_close(&dec);
        return 1;
    }
    const double actual_hz = mode_refresh(display.mode);
    logf("mode selected index=%d name=%s %ux%u clock=%u refresh=%.3f (requested %.3f, delta %.3f) "
         "flags=0x%x",
         mode_index, display.mode.name, display.mode.hdisplay, display.mode.vdisplay,
         display.mode.clock, actual_hz, opt.target_hz, actual_hz - opt.target_hz,
         display.mode.flags);
    if (std::fabs(actual_hz - opt.target_hz) > 0.01)
        logf("mode NOTE: exact %.3f Hz mode unavailable; using %.3f Hz instead", opt.target_hz,
             actual_hz);

    drmModeEncoder *encoder = drmModeGetEncoder(display.fd, conn->encoder_id
                                                                ? conn->encoder_id
                                                                : conn->encoders[0]);
    if (!encoder) fail("drmModeGetEncoder");
    for (int i = 0; i < res->count_crtcs; ++i) {
        if (encoder->possible_crtcs & (1u << i)) {
            display.crtc_id = res->crtcs[i];
            display.crtc_index = i;
            break;
        }
    }
    drmModeFreeEncoder(encoder);
    if (display.crtc_index < 0) fail("no usable CRTC for HDMI encoder");

    {
        drmModePlaneRes *all = drmModeGetPlaneResources(display.fd);
        if (all) {
            for (uint32_t i = 0; i < all->count_planes; ++i) {
                drmModePlane *p = drmModeGetPlane(display.fd, all->planes[i]);
                if (!p) continue;
                if (p->crtc_id == display.crtc_id) {
                    display.stale_planes.push_back(
                        {p->plane_id,
                         require_property(display.fd, p->plane_id, DRM_MODE_OBJECT_PLANE, "FB_ID"),
                         require_property(display.fd, p->plane_id, DRM_MODE_OBJECT_PLANE,
                                          "CRTC_ID")});
                    logf("inherited plane id=%u is bound to CRTC %u; it will be disabled",
                         p->plane_id, display.crtc_id);
                }
                drmModeFreePlane(p);
            }
            drmModeFreePlaneResources(all);
        }
    }
    display.p_conn_crtc =
        require_property(display.fd, display.connector_id, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID");
    display.p_crtc_active =
        require_property(display.fd, display.crtc_id, DRM_MODE_OBJECT_CRTC, "ACTIVE");
    display.p_crtc_mode =
        require_property(display.fd, display.crtc_id, DRM_MODE_OBJECT_CRTC, "MODE_ID");
    display.colorspace =
        lookup_property(display.fd, display.connector_id, DRM_MODE_OBJECT_CONNECTOR, "Colorspace");
    display.color_depth =
        lookup_property(display.fd, display.connector_id, DRM_MODE_OBJECT_CONNECTOR, "color_depth");
    display.hdr_metadata = lookup_property(display.fd, display.connector_id,
                                           DRM_MODE_OBJECT_CONNECTOR, "HDR_OUTPUT_METADATA");
    display.saved_colorspace = display.colorspace.value;
    display.saved_color_depth = display.color_depth.value;
    display.saved_connector_state = true;
    logf("connector properties Colorspace=%d(value %" PRIu64 ") color_depth=%d(value %" PRIu64
         ") HDR_OUTPUT_METADATA=%d; these values will be restored on exit",
         display.colorspace.present, display.saved_colorspace, display.color_depth.present,
         display.saved_color_depth, display.hdr_metadata.present);

    if (drmModeCreatePropertyBlob(display.fd, &display.mode, sizeof(display.mode),
                                  &display.mode_blob_id))
        fail("drmModeCreatePropertyBlob(mode)");

    // ---- first decoded frame, needed before a plane can be tested -----------
    AVPacket *packet = av_packet_alloc();
    AVFrame *frame = av_frame_alloc();
    if (!packet || !frame) fail("av_packet_alloc/av_frame_alloc");

    FramebufferCache framebuffers(display.fd);
    static FramebufferCache *release_target = nullptr;
    release_target = &framebuffers;
    g_release_framebuffers = [] {
        if (release_target) release_target->clear();
    };
    uint32_t first_fb = 0;
    int decode_errors = 0;
    long frames_decoded = 0;

    auto pump_one_frame = [&](bool *eof) -> bool {
        for (;;) {
            const int ret = avcodec_receive_frame(dec.codec, frame);
            if (ret == 0) {
                ++frames_decoded;
                return true;
            }
            if (ret == AVERROR_EOF) {
                *eof = true;
                return false;
            }
            if (ret != AVERROR(EAGAIN)) {
                ++decode_errors;
                return false;
            }
            const int read = av_read_frame(dec.fmt, packet);
            if (read < 0) {
                avcodec_send_packet(dec.codec, nullptr);
                continue;
            }
            if (packet->stream_index != dec.stream_index) {
                av_packet_unref(packet);
                continue;
            }
            if (avcodec_send_packet(dec.codec, packet) < 0) ++decode_errors;
            av_packet_unref(packet);
        }
    };

    bool eof = false;
    if (!pump_one_frame(&eof)) {
        logf("STEP %s RESULT: BLOCKED (no frame decoded; decode_errors=%d)", step->name,
             decode_errors);
        av_frame_free(&frame);
        av_packet_free(&packet);
        drmModeFreeConnector(conn);
        drmModeFreeResources(res);
        restore_display();
        decoder_close(&dec);
        return 1;
    }
    if (frame->format != AV_PIX_FMT_DRM_PRIME) {
        logf("STEP %s RESULT: BLOCKED (decoder returned %s, not drm_prime; refusing any CPU "
             "conversion)",
             step->name, av_get_pix_fmt_name(static_cast<AVPixelFormat>(frame->format)));
        av_frame_free(&frame);
        av_packet_free(&packet);
        drmModeFreeConnector(conn);
        drmModeFreeResources(res);
        restore_display();
        decoder_close(&dec);
        return 1;
    }
    const AVDRMFrameDescriptor *desc =
        reinterpret_cast<const AVDRMFrameDescriptor *>(frame->data[0]);
    char got_fmt[5];
    fourcc_string(desc->layers[0].format, got_fmt);
    logf("decoder drm_prime layer_format=%s modifier=0x%" PRIx64 " objects=%d planes=%d",
         got_fmt, desc->objects[0].format_modifier, desc->nb_objects, desc->layers[0].nb_planes);
    if (desc->layers[0].format != step->format) {
        logf("STEP %s RESULT: BLOCKED (decoder produced %s but step requires %s; conversion is "
             "forbidden in this gate)",
             step->name, got_fmt, want_fmt);
        av_frame_free(&frame);
        av_packet_free(&packet);
        drmModeFreeConnector(conn);
        drmModeFreeResources(res);
        restore_display();
        decoder_close(&dec);
        return 1;
    }
    framebuffers.retain_in_flight(frame);
    if (!framebuffers.get(desc, frame, &first_fb, &error)) {
        logf("STEP %s RESULT: FAIL (framebuffer import: %s)", step->name, error.c_str());
        av_frame_free(&frame);
        av_packet_free(&packet);
        drmModeFreeConnector(conn);
        drmModeFreeResources(res);
        restore_display();
        decoder_close(&dec);
        return 1;
    }

    // Frame-level side data is authoritative when present: it is what the
    // decoder actually parsed out of this stream.
    if (const AVFrameSideData *sd =
            av_frame_get_side_data(frame, AV_FRAME_DATA_MASTERING_DISPLAY_METADATA)) {
        hdr_source.mastering = *reinterpret_cast<const AVMasteringDisplayMetadata *>(sd->data);
        hdr_source.have_mastering = true;
    }
    if (const AVFrameSideData *sd =
            av_frame_get_side_data(frame, AV_FRAME_DATA_CONTENT_LIGHT_LEVEL)) {
        hdr_source.light = *reinterpret_cast<const AVContentLightMetadata *>(sd->data);
        hdr_source.have_light_level = true;
    }
    if (frame->color_trc != AVCOL_TRC_UNSPECIFIED) hdr_source.trc = frame->color_trc;
    logf("asset hdr_side_data mastering=%d content_light_level=%d transfer=%s",
         hdr_source.have_mastering, hdr_source.have_light_level,
         av_color_transfer_name(hdr_source.trc));

    if (step->set_hdr_metadata) {
        if (!display.hdr_metadata.present) {
            logf("STEP %s RESULT: BLOCKED (connector has no HDR_OUTPUT_METADATA property)",
                 step->name);
            drmModeFreeConnector(conn);
            drmModeFreeResources(res);
            restore_display();
            decoder_close(&dec);
            return 1;
        }
        struct hdr_output_metadata metadata {};
        std::string notes;
        if (!build_hdr_metadata(hdr_source, &metadata, &notes)) {
            logf("STEP %s RESULT: BLOCKED (%s)", step->name, notes.c_str());
            drmModeFreeConnector(conn);
            drmModeFreeResources(res);
            restore_display();
            decoder_close(&dec);
            return 1;
        }
        if (!notes.empty()) logf("hdr_metadata provenance notes: %s", notes.c_str());
        log_hdr_metadata(metadata);
        if (drmModeCreatePropertyBlob(display.fd, &metadata, sizeof(metadata),
                                      &display.hdr_blob_id)) {
            logf("STEP %s RESULT: FAIL (drmModeCreatePropertyBlob(hdr): %s)", step->name,
                 strerror(errno));
            drmModeFreeConnector(conn);
            drmModeFreeResources(res);
            restore_display();
            decoder_close(&dec);
            return 1;
        }
        logf("hdr_metadata blob_id=%u", display.hdr_blob_id);
    }


    // ---- plane selection by atomic TEST_ONLY, not by plane type -------------
    drmModePlaneRes *plane_res = drmModeGetPlaneResources(display.fd);
    if (!plane_res) fail("drmModeGetPlaneResources");
    bool plane_found = false;
    for (uint32_t i = 0; i < plane_res->count_planes && !plane_found; ++i) {
        drmModePlane *plane = drmModeGetPlane(display.fd, plane_res->planes[i]);
        if (!plane) continue;
        const bool candidate =
            (plane->possible_crtcs & (1u << display.crtc_index)) &&
            plane_has_format(plane, step->format) &&
            (!opt.forced_plane || opt.forced_plane == plane->plane_id);
        if (candidate) {
            PropertyInfo type =
                lookup_property(display.fd, plane->plane_id, DRM_MODE_OBJECT_PLANE, "type");
            display.plane_id = plane->plane_id;
            display.plane_type = static_cast<uint32_t>(type.value);
            bind_plane_properties(display);

            drmModeAtomicReq *req = drmModeAtomicAlloc();
            build_request(req, display, *step, first_fb, true);
            const int test = drmModeAtomicCommit(
                display.fd, req, DRM_MODE_ATOMIC_TEST_ONLY | DRM_MODE_ATOMIC_ALLOW_MODESET,
                nullptr);
            drmModeAtomicFree(req);
            logf("plane candidate id=%u type=%s test_only=%s", plane->plane_id,
                 type.present ? enum_name_for(type, type.value) : "?",
                 test == 0 ? "ACCEPTED" : strerror(-test));
            if (test == 0) plane_found = true;
            else display.plane_id = 0;
        }
        drmModeFreePlane(plane);
    }
    drmModeFreePlaneResources(plane_res);

    if (!plane_found) {
        logf("STEP %s RESULT: FAIL (no plane on CRTC %u accepted format %s in an atomic test)",
             step->name, display.crtc_id, want_fmt);
        av_frame_free(&frame);
        av_packet_free(&packet);
        drmModeFreeConnector(conn);
        drmModeFreeResources(res);
        restore_display();
        decoder_close(&dec);
        return 1;
    }
    logf("plane selected id=%u", display.plane_id);

    // ---- real commits -------------------------------------------------------
    dump_file("/sys/kernel/debug/dri/0/summary", "debugfs summary BEFORE step");

    drmEventContext event_context{};
    event_context.version = 2;
    event_context.page_flip_handler = page_flip_handler;
    FlipState flip;

    auto commit = [&](uint32_t fb_id, bool modeset) -> int {
        drmModeAtomicReq *req = drmModeAtomicAlloc();
        build_request(req, display, *step, fb_id, modeset);
        uint32_t flags = DRM_MODE_PAGE_FLIP_EVENT;
        if (modeset) flags |= DRM_MODE_ATOMIC_ALLOW_MODESET;
        flip.pending = true;
        const int ret = drmModeAtomicCommit(display.fd, req, flags, &flip);
        drmModeAtomicFree(req);
        if (ret) {
            flip.pending = false;
            return ret;
        }
        display.last_fb_id = fb_id;
        while (flip.pending) {
            struct pollfd pfd {};
            pfd.fd = display.fd;
            pfd.events = POLLIN;
            const int ready = poll(&pfd, 1, 2000);
            if (ready < 0 && errno == EINTR) continue;
            if (ready <= 0) {
                logf("warning: page-flip event wait timed out");
                flip.pending = false;
                break;
            }
            drmHandleEvent(display.fd, &event_context);
        }
        return 0;
    };

    const int modeset_ret = commit(first_fb, true);
    if (modeset_ret) {
        logf("STEP %s RESULT: FAIL (atomic modeset commit: %s)", step->name, strerror(-modeset_ret));
        av_frame_free(&frame);
        av_packet_free(&packet);
        drmModeFreeConnector(conn);
        drmModeFreeResources(res);
        restore_display();
        decoder_close(&dec);
        return 1;
    }
    logf("atomic modeset committed");
    dump_file("/sys/kernel/debug/dri/0/summary", "debugfs summary AFTER modeset");
    report_readback(display, *step);

    // ---- hold: loop the asset for the requested wall time -------------------
    const time_t deadline = time(nullptr) + opt.hold_seconds;
    long flips = 0;
    int commit_errors = 0;
    while (time(nullptr) < deadline) {
        eof = false;
        if (!pump_one_frame(&eof)) {
            if (!eof && decode_errors) break;
            // Restart the asset so the mode stays up for the whole hold window.
            avcodec_flush_buffers(dec.codec);
            if (av_seek_frame(dec.fmt, dec.stream_index, 0, AVSEEK_FLAG_BACKWARD) < 0) break;
            continue;
        }
        if (frame->format != AV_PIX_FMT_DRM_PRIME) {
            ++decode_errors;
            break;
        }
        const AVDRMFrameDescriptor *fd_desc =
            reinterpret_cast<const AVDRMFrameDescriptor *>(frame->data[0]);
        uint32_t fb_id = 0;
        if (!framebuffers.get(fd_desc, frame, &fb_id, &error)) {
            logf("framebuffer import failed mid-run: %s", error.c_str());
            ++commit_errors;
            break;
        }
        framebuffers.retain_in_flight(frame);
        const int ret = commit(fb_id, false);
        if (ret) {
            logf("atomic page-flip commit failed: %s", strerror(-ret));
            ++commit_errors;
            break;
        }
        ++flips;
        if (flips % 24 == 0)
            logf("progress flips=%ld frames_decoded=%ld imported=%zu", flips, frames_decoded,
                 framebuffers.size());
        av_frame_unref(frame);
    }

    dump_file("/sys/kernel/debug/dri/0/summary", "debugfs summary DURING hold (end of run)");
    report_readback(display, *step);
    logf("run frames_decoded=%ld page_flips=%ld decode_errors=%d commit_errors=%d "
         "imported_framebuffers=%zu",
         frames_decoded, flips, decode_errors, commit_errors, framebuffers.size());

    const bool ok = flips > 0 && decode_errors == 0 && commit_errors == 0;
    logf("STEP %s RESULT: %s", step->name, ok ? "PASS" : "FAIL");

    av_frame_free(&frame);
    av_packet_free(&packet);
    drmModeFreeConnector(conn);
    drmModeFreeResources(res);
    restore_display();
    decoder_close(&dec);
    return ok ? 0 : 1;
}

// The connector's colour properties are sticky and have no console owner: the
// kernel fbdev client restores its own mode when a master leaves but never
// touches Colorspace, color_depth or HDR_OUTPUT_METADATA. A run therefore
// restores what it found, and --reset exists to put the link back to a neutral
// SDR state afterwards.
int run_reset(const char *card) {
    Display display;
    g_display = &display;
    display.fd = open_card(card);
    if (drmSetMaster(display.fd)) {
        logf("RESET RESULT: BLOCKED (drmSetMaster: %s)", strerror(errno));
        restore_display();
        return 1;
    }
    display.master = true;

    drmModeRes *res = drmModeGetResources(display.fd);
    if (!res) fail("drmModeGetResources");
    drmModeConnector *conn = nullptr;
    for (int i = 0; i < res->count_connectors && !conn; ++i) {
        drmModeConnector *c = drmModeGetConnector(display.fd, res->connectors[i]);
        if (c && c->connector_type == DRM_MODE_CONNECTOR_HDMIA &&
            c->connection == DRM_MODE_CONNECTED && c->encoder_id) {
            conn = c;
            break;
        }
        if (c) drmModeFreeConnector(c);
    }
    if (!conn) {
        logf("RESET RESULT: BLOCKED (no active HDMI-A connector)");
        drmModeFreeResources(res);
        restore_display();
        return 1;
    }
    drmModeEncoder *encoder = drmModeGetEncoder(display.fd, conn->encoder_id);
    if (!encoder || !encoder->crtc_id) {
        logf("RESET RESULT: BLOCKED (connector %u has no active CRTC)", conn->connector_id);
        if (encoder) drmModeFreeEncoder(encoder);
        drmModeFreeConnector(conn);
        drmModeFreeResources(res);
        restore_display();
        return 1;
    }
    display.connector_id = conn->connector_id;
    display.crtc_id = encoder->crtc_id;
    drmModeFreeEncoder(encoder);

    drmModeCrtc *crtc = drmModeGetCrtc(display.fd, display.crtc_id);
    if (!crtc || !crtc->mode_valid) {
        logf("RESET RESULT: BLOCKED (CRTC %u has no valid mode)", display.crtc_id);
        if (crtc) drmModeFreeCrtc(crtc);
        drmModeFreeConnector(conn);
        drmModeFreeResources(res);
        restore_display();
        return 1;
    }
    display.mode = crtc->mode;
    drmModeFreeCrtc(crtc);
    if (drmModeCreatePropertyBlob(display.fd, &display.mode, sizeof(display.mode),
                                  &display.mode_blob_id))
        fail("drmModeCreatePropertyBlob(mode)");

    display.p_conn_crtc =
        require_property(display.fd, display.connector_id, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID");
    display.p_crtc_active =
        require_property(display.fd, display.crtc_id, DRM_MODE_OBJECT_CRTC, "ACTIVE");
    display.p_crtc_mode =
        require_property(display.fd, display.crtc_id, DRM_MODE_OBJECT_CRTC, "MODE_ID");
    display.colorspace =
        lookup_property(display.fd, display.connector_id, DRM_MODE_OBJECT_CONNECTOR, "Colorspace");
    display.color_depth =
        lookup_property(display.fd, display.connector_id, DRM_MODE_OBJECT_CONNECTOR, "color_depth");
    display.hdr_metadata = lookup_property(display.fd, display.connector_id,
                                           DRM_MODE_OBJECT_CONNECTOR, "HDR_OUTPUT_METADATA");

    drmModeAtomicReq *req = drmModeAtomicAlloc();
    add_prop(req, display.connector_id, display.p_conn_crtc, display.crtc_id);
    add_prop(req, display.crtc_id, display.p_crtc_active, 1);
    add_prop(req, display.crtc_id, display.p_crtc_mode, display.mode_blob_id);
    if (display.colorspace.present) {
        auto it = display.colorspace.enums.find("Default");
        if (it != display.colorspace.enums.end())
            add_prop(req, display.connector_id, display.colorspace.id, it->second);
    }
    if (display.color_depth.present) {
        auto it = display.color_depth.enums.find("Automatic");
        if (it != display.color_depth.enums.end())
            add_prop(req, display.connector_id, display.color_depth.id, it->second);
    }
    if (display.hdr_metadata.present)
        add_prop(req, display.connector_id, display.hdr_metadata.id, 0);
    const int ret =
        drmModeAtomicCommit(display.fd, req, DRM_MODE_ATOMIC_ALLOW_MODESET, nullptr);
    drmModeAtomicFree(req);

    display.saved_connector_state = false;  // nothing to put back; this is the neutral state
    if (ret) {
        logf("RESET RESULT: FAIL (atomic commit: %s)", strerror(-ret));
    } else {
        logf("reset connector %u to Colorspace=Default color_depth=Automatic "
             "HDR_OUTPUT_METADATA=unset on mode %s",
             display.connector_id, display.mode.name);
    }
    dump_file("/sys/kernel/debug/dri/0/summary", "debugfs summary AFTER reset");
    drmModeFreeConnector(conn);
    drmModeFreeResources(res);
    restore_display();
    logf("RESET RESULT: %s", ret ? "FAIL" : "PASS");
    return ret ? 1 : 0;
}

void usage() {
    printf(
        "usage:\n"
        "  hdr-signaling-probe --probe [--card /dev/dri/card0]\n"
        "  hdr-signaling-probe --step a0|a1|a2|a3|a4 --asset FILE [options]\n"
        "  hdr-signaling-probe --reset\n"
        "\n"
        "options:\n"
        "  --card PATH        DRM card (default /dev/dri/card0)\n"
        "  --decoder NAME     libavcodec decoder (default hevc_rkmpp)\n"
        "  --hold SECONDS     how long to keep the mode up (default 20)\n"
        "  --refresh HZ       target refresh rate (default 23.976)\n"
        "  --plane ID         restrict plane search to this plane id\n"
        "\n"
        "--probe performs no modeset. Each --step prints PASS, FAIL or BLOCKED.\n"
        "--reset returns Colorspace, color_depth and HDR_OUTPUT_METADATA to their\n"
        "neutral SDR values; these are sticky and no console client clears them.\n");
}

}  // namespace

int main(int argc, char **argv) {
    Options opt;
    for (int i = 1; i < argc; ++i) {
        const char *arg = argv[i];
        auto next = [&]() -> const char * {
            if (i + 1 >= argc) {
                fprintf(stderr, "error: %s needs a value\n", arg);
                exit(2);
            }
            return argv[++i];
        };
        if (!strcmp(arg, "--probe")) opt.probe_only = true;
        else if (!strcmp(arg, "--reset")) opt.reset_only = true;
        else if (!strcmp(arg, "--step")) opt.step = next();
        else if (!strcmp(arg, "--asset")) opt.asset = next();
        else if (!strcmp(arg, "--card")) opt.card = next();
        else if (!strcmp(arg, "--decoder")) opt.decoder = next();
        else if (!strcmp(arg, "--hold")) opt.hold_seconds = atoi(next());
        else if (!strcmp(arg, "--refresh")) opt.target_hz = atof(next());
        else if (!strcmp(arg, "--plane")) opt.forced_plane = strtoul(next(), nullptr, 0);
        else if (!strcmp(arg, "--help") || !strcmp(arg, "-h")) {
            usage();
            return 0;
        } else {
            fprintf(stderr, "error: unknown argument %s\n", arg);
            usage();
            return 2;
        }
    }

    if (opt.probe_only) return run_probe(opt.card);
    if (opt.reset_only) {
        install_signal_handlers();
        alarm(60);
        return run_reset(opt.card);
    }
    if (!opt.step) {
        usage();
        return 2;
    }
    install_signal_handlers();
    // Hold window, plus decode start-up and teardown margin.
    alarm(static_cast<unsigned>(opt.hold_seconds) + 90u);
    g_exit_code = run_step(opt);
    alarm(0);
    return g_exit_code;
}
