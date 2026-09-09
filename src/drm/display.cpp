#include "drm/display.h"

#include "common/log.h"

#include <cerrno>
#include <cinttypes>
#include <cmath>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <cstring>

#include <fcntl.h>
#include <poll.h>
#include <unistd.h>

#include <drm_fourcc.h>
#include <drm_mode.h>

namespace mediabox::drm {
namespace {

Display *g_restore_target = nullptr;

bool plane_has_format(const drmModePlane *plane, uint32_t format) {
    for (uint32_t i = 0; i < plane->count_formats; ++i)
        if (plane->formats[i] == format) return true;
    return false;
}

int open_card(const char *path, std::string *error) {
    const int fd = ::open(path, O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        *error = std::string("open ") + path + ": " + strerror(errno);
        return -1;
    }
    if (drmSetClientCap(fd, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1)) {
        *error = std::string("drmSetClientCap(UNIVERSAL_PLANES): ") + strerror(errno);
        ::close(fd);
        return -1;
    }
    if (drmSetClientCap(fd, DRM_CLIENT_CAP_ATOMIC, 1)) {
        *error = std::string("drmSetClientCap(ATOMIC): ") + strerror(errno);
        ::close(fd);
        return -1;
    }
    return fd;
}

void add_prop(drmModeAtomicReq *req, uint32_t object, uint32_t property, uint64_t value) {
    if (drmModeAtomicAddProperty(req, object, property, value) < 0)
        mediabox::fail("drmModeAtomicAddProperty");
}

void on_signal(int sig) {
    // Async-signal-safe enough for a diagnostic tool: put the display back and
    // leave. A probe that dies holding DRM master leaves the console dark.
    if (g_restore_target) g_restore_target->restore();
    _exit(128 + sig);
}

}  // namespace

void page_flip_trampoline(int, unsigned int sequence, unsigned int tv_sec, unsigned int tv_usec,
                          void *user) {
    auto *wait = static_cast<Display::FlipWait *>(user);
    wait->pending = false;
    wait->info.sequence = sequence;
    wait->info.timestamp =
        static_cast<double>(tv_sec) + static_cast<double>(tv_usec) / 1e6;
    wait->info.valid = true;
}

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
            for (int e = 0; e < p->count_enums; ++e) info.enums[p->enums[e].name] = p->enums[e].value;
        }
        drmModeFreeProperty(p);
    }
    drmModeFreeObjectProperties(props);
    return info;
}

uint32_t require_property(int fd, uint32_t object, uint32_t type, const char *name) {
    const PropertyInfo info = lookup_property(fd, object, type, name);
    if (!info.present) {
        fprintf(stderr, "error: DRM object %u lacks required property \"%s\"\n", object, name);
        _exit(EXIT_FAILURE);
    }
    return info.id;
}

const char *enum_name_for(const PropertyInfo &info, uint64_t value) {
    for (const auto &entry : info.enums)
        if (entry.second == value) return entry.first.c_str();
    return "<not-an-enum-value>";
}

double mode_refresh(const drmModeModeInfo &m) {
    if (!m.htotal || !m.vtotal) return 0.0;
    double refresh = static_cast<double>(m.clock) * 1000.0 /
                     (static_cast<double>(m.htotal) * static_cast<double>(m.vtotal));
    if (m.flags & DRM_MODE_FLAG_INTERLACE) refresh *= 2.0;
    return refresh;
}

void set_restore_target(Display *display) { g_restore_target = display; }

void install_signal_handlers() {
    struct sigaction sa {};
    sa.sa_handler = on_signal;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGINT, &sa, nullptr);
    sigaction(SIGTERM, &sa, nullptr);
    sigaction(SIGHUP, &sa, nullptr);
    // The watchdog: a probe that wedges while holding DRM master must still
    // give the console back. main() arms alarm() with the real budget.
    sigaction(SIGALRM, &sa, nullptr);
    signal(SIGPIPE, SIG_IGN);
}

bool Display::open(const char *card, std::string *error) {
    fd_ = open_card(card, error);
    if (fd_ < 0) return false;
    if (drmSetMaster(fd_)) {
        *error = std::string("drmSetMaster: ") + strerror(errno);
        return false;
    }
    master_ = true;
    return true;
}

bool Display::discover(double target_hz, std::string *error) {
    drmModeRes *res = drmModeGetResources(fd_);
    if (!res) {
        *error = "drmModeGetResources failed";
        return false;
    }
    drmModeConnector *conn = nullptr;
    for (int i = 0; i < res->count_connectors && !conn; ++i) {
        drmModeConnector *c = drmModeGetConnector(fd_, res->connectors[i]);
        if (c && c->connector_type == DRM_MODE_CONNECTOR_HDMIA &&
            c->connection == DRM_MODE_CONNECTED && c->count_modes > 0) {
            conn = c;
            break;
        }
        if (c) drmModeFreeConnector(c);
    }
    if (!conn) {
        *error = "no connected HDMI-A connector with modes";
        drmModeFreeResources(res);
        return false;
    }
    connector_id_ = conn->connector_id;

    // Closest 3840x2160 progressive mode. Reported with its error so a report
    // can state plainly whether the requested cadence was actually available.
    double best_error = 1e9;
    int best = -1;
    for (int m = 0; m < conn->count_modes; ++m) {
        const drmModeModeInfo &mm = conn->modes[m];
        if (mm.hdisplay != 3840 || mm.vdisplay != 2160) continue;
        if (mm.flags & DRM_MODE_FLAG_INTERLACE) continue;
        const double err = std::fabs(mode_refresh(mm) - target_hz);
        if (err < best_error) {
            best_error = err;
            best = m;
        }
    }
    if (best < 0) {
        *error = "no progressive 3840x2160 mode on this connector";
        drmModeFreeConnector(conn);
        drmModeFreeResources(res);
        return false;
    }
    mode_ = conn->modes[best];
    const double actual = mode_refresh(mode_);
    mediabox::logf("mode selected index=%d name=%s %ux%u clock=%u refresh=%.4f "
                   "(requested %.4f, delta %+.4f) flags=0x%x",
                   best, mode_.name, mode_.hdisplay, mode_.vdisplay, mode_.clock, actual,
                   target_hz, actual - target_hz, mode_.flags);

    drmModeEncoder *encoder =
        drmModeGetEncoder(fd_, conn->encoder_id ? conn->encoder_id : conn->encoders[0]);
    if (!encoder) {
        *error = "drmModeGetEncoder failed";
        drmModeFreeConnector(conn);
        drmModeFreeResources(res);
        return false;
    }
    for (int i = 0; i < res->count_crtcs; ++i) {
        if (encoder->possible_crtcs & (1u << i)) {
            crtc_id_ = res->crtcs[i];
            crtc_index_ = i;
            break;
        }
    }
    drmModeFreeEncoder(encoder);
    drmModeFreeConnector(conn);
    drmModeFreeResources(res);
    if (crtc_index_ < 0) {
        *error = "no usable CRTC for the HDMI encoder";
        return false;
    }

    drmModePlaneRes *all = drmModeGetPlaneResources(fd_);
    if (all) {
        for (uint32_t i = 0; i < all->count_planes; ++i) {
            drmModePlane *p = drmModeGetPlane(fd_, all->planes[i]);
            if (!p) continue;
            if (p->crtc_id == crtc_id_) {
                stale_planes_.push_back(
                    {p->plane_id, require_property(fd_, p->plane_id, DRM_MODE_OBJECT_PLANE, "FB_ID"),
                     require_property(fd_, p->plane_id, DRM_MODE_OBJECT_PLANE, "CRTC_ID")});
                mediabox::logf("inherited plane id=%u is bound to CRTC %u; it will be disabled",
                               p->plane_id, crtc_id_);
            }
            drmModeFreePlane(p);
        }
        drmModeFreePlaneResources(all);
    }

    p_conn_crtc_ = require_property(fd_, connector_id_, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID");
    p_crtc_active_ = require_property(fd_, crtc_id_, DRM_MODE_OBJECT_CRTC, "ACTIVE");
    p_crtc_mode_ = require_property(fd_, crtc_id_, DRM_MODE_OBJECT_CRTC, "MODE_ID");
    colorspace_ = lookup_property(fd_, connector_id_, DRM_MODE_OBJECT_CONNECTOR, "Colorspace");
    color_depth_ = lookup_property(fd_, connector_id_, DRM_MODE_OBJECT_CONNECTOR, "color_depth");
    hdr_metadata_ =
        lookup_property(fd_, connector_id_, DRM_MODE_OBJECT_CONNECTOR, "HDR_OUTPUT_METADATA");
    saved_colorspace_ = colorspace_.value;
    saved_color_depth_ = color_depth_.value;
    saved_connector_state_ = true;
    mediabox::logf("connector %u properties Colorspace=%d(value %" PRIu64
                   ") color_depth=%d(value %" PRIu64 ") HDR_OUTPUT_METADATA=%d; "
                   "these values will be restored on exit",
                   connector_id_, colorspace_.present, saved_colorspace_, color_depth_.present,
                   saved_color_depth_, hdr_metadata_.present);

    if (drmModeCreatePropertyBlob(fd_, &mode_, sizeof(mode_), &mode_blob_id_)) {
        *error = std::string("drmModeCreatePropertyBlob(mode): ") + strerror(errno);
        return false;
    }
    return true;
}

bool Display::create_hdr_blob(const void *payload, size_t size, uint32_t *blob_id,
                              std::string *error) {
    if (drmModeCreatePropertyBlob(fd_, payload, size, blob_id)) {
        *error = std::string("drmModeCreatePropertyBlob(hdr): ") + strerror(errno);
        return false;
    }
    hdr_blob_id_ = *blob_id;
    return true;
}

void Display::bind_plane_properties() {
    p_plane_fb_ = require_property(fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "FB_ID");
    p_plane_crtc_ = require_property(fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "CRTC_ID");
    p_src_x_ = require_property(fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "SRC_X");
    p_src_y_ = require_property(fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "SRC_Y");
    p_src_w_ = require_property(fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "SRC_W");
    p_src_h_ = require_property(fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "SRC_H");
    p_crtc_x_ = require_property(fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "CRTC_X");
    p_crtc_y_ = require_property(fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "CRTC_Y");
    p_crtc_w_ = require_property(fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "CRTC_W");
    p_crtc_h_ = require_property(fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "CRTC_H");
}

void Display::build_request(drmModeAtomicReq *req, const OutputState &state, uint32_t fb_id,
                            bool modeset, const Rect &src, const Rect &dst) const {
    if (modeset) {
        add_prop(req, connector_id_, p_conn_crtc_, crtc_id_);
        add_prop(req, crtc_id_, p_crtc_active_, 1);
        add_prop(req, crtc_id_, p_crtc_mode_, mode_blob_id_);
        for (const StalePlane &stale : stale_planes_) {
            if (stale.id == plane_id_) continue;
            add_prop(req, stale.id, stale.fb_prop, 0);
            add_prop(req, stale.id, stale.crtc_prop, 0);
        }
    }
    if (colorspace_.present) {
        const char *want = state.bt2020_ycc ? "BT2020_YCC" : "Default";
        auto it = colorspace_.enums.find(want);
        if (it != colorspace_.enums.end()) add_prop(req, connector_id_, colorspace_.id, it->second);
    }
    if (color_depth_.present) {
        const char *want = state.depth30 ? "30bit" : "Automatic";
        auto it = color_depth_.enums.find(want);
        if (it != color_depth_.enums.end()) add_prop(req, connector_id_, color_depth_.id, it->second);
    }
    if (hdr_metadata_.present)
        add_prop(req, connector_id_, hdr_metadata_.id, state.hdr_blob_id);

    add_prop(req, plane_id_, p_plane_fb_, fb_id);
    add_prop(req, plane_id_, p_plane_crtc_, crtc_id_);
    add_prop(req, plane_id_, p_src_x_, static_cast<uint64_t>(src.x) << 16);
    add_prop(req, plane_id_, p_src_y_, static_cast<uint64_t>(src.y) << 16);
    add_prop(req, plane_id_, p_src_w_, static_cast<uint64_t>(src.w) << 16);
    add_prop(req, plane_id_, p_src_h_, static_cast<uint64_t>(src.h) << 16);
    add_prop(req, plane_id_, p_crtc_x_, dst.x);
    add_prop(req, plane_id_, p_crtc_y_, dst.y);
    add_prop(req, plane_id_, p_crtc_w_, dst.w);
    add_prop(req, plane_id_, p_crtc_h_, dst.h);
}

bool Display::select_plane(uint32_t fourcc, uint32_t forced_plane, const OutputState &state,
                           uint32_t first_fb, const Rect &src, const Rect &dst,
                           std::string *error) {
    drmModePlaneRes *plane_res = drmModeGetPlaneResources(fd_);
    if (!plane_res) {
        *error = "drmModeGetPlaneResources failed";
        return false;
    }
    bool found = false;
    for (uint32_t i = 0; i < plane_res->count_planes && !found; ++i) {
        drmModePlane *plane = drmModeGetPlane(fd_, plane_res->planes[i]);
        if (!plane) continue;
        const bool candidate = (plane->possible_crtcs & (1u << crtc_index_)) &&
                               plane_has_format(plane, fourcc) &&
                               (!forced_plane || forced_plane == plane->plane_id);
        if (candidate) {
            const PropertyInfo type =
                lookup_property(fd_, plane->plane_id, DRM_MODE_OBJECT_PLANE, "type");
            plane_id_ = plane->plane_id;
            plane_type_name_ = type.present ? enum_name_for(type, type.value) : "?";
            bind_plane_properties();

            drmModeAtomicReq *req = drmModeAtomicAlloc();
            build_request(req, state, first_fb, true, src, dst);
            const int test = drmModeAtomicCommit(
                fd_, req, DRM_MODE_ATOMIC_TEST_ONLY | DRM_MODE_ATOMIC_ALLOW_MODESET, nullptr);
            drmModeAtomicFree(req);
            mediabox::logf("plane candidate id=%u type=%s test_only=%s", plane->plane_id,
                           plane_type_name_.c_str(), test == 0 ? "ACCEPTED" : strerror(-test));
            if (test == 0) found = true;
            else plane_id_ = 0;
        }
        drmModeFreePlane(plane);
    }
    drmModeFreePlaneResources(plane_res);
    if (!found) {
        char fmt[5];
        mediabox::fourcc_string(fourcc, fmt);
        *error = std::string("no plane on CRTC accepted format ") + fmt + " in an atomic test";
        return false;
    }
    mediabox::logf("plane selected id=%u type=%s", plane_id_, plane_type_name_.c_str());
    return true;
}

int Display::submit(uint32_t fb_id, bool modeset, const OutputState &state, const Rect &src,
                    const Rect &dst) {
    drmModeAtomicReq *req = drmModeAtomicAlloc();
    build_request(req, state, fb_id, modeset, src, dst);
    uint32_t flags = DRM_MODE_PAGE_FLIP_EVENT;
    if (modeset) flags |= DRM_MODE_ATOMIC_ALLOW_MODESET;
    flip_.pending = true;
    flip_.info.valid = false;
    const int ret = drmModeAtomicCommit(fd_, req, flags, &flip_);
    drmModeAtomicFree(req);
    if (ret) {
        flip_.pending = false;
        return ret;
    }
    last_fb_id_ = fb_id;
    last_src_ = src;
    last_dst_ = dst;
    last_state_ = state;
    return 0;
}

bool Display::wait_flip(FlipInfo *out, int timeout_ms) {
    drmEventContext ctx{};
    ctx.version = 2;
    ctx.page_flip_handler = page_flip_trampoline;
    while (flip_.pending) {
        struct pollfd pfd {};
        pfd.fd = fd_;
        pfd.events = POLLIN;
        const int ready = poll(&pfd, 1, timeout_ms);
        if (ready < 0 && errno == EINTR) continue;
        if (ready <= 0) {
            flip_.pending = false;
            return false;
        }
        drmHandleEvent(fd_, &ctx);
    }
    if (out) *out = flip_.info;
    return flip_.info.valid;
}

void Display::report_readback(const OutputState &state) const {
    mediabox::logf("---- requested vs actual ----");
    if (colorspace_.present) {
        const PropertyInfo now =
            lookup_property(fd_, connector_id_, DRM_MODE_OBJECT_CONNECTOR, "Colorspace");
        mediabox::logf("Colorspace          requested=%s actual=%s (%" PRIu64 ")",
                       state.bt2020_ycc ? "BT2020_YCC" : "Default", enum_name_for(now, now.value),
                       now.value);
    }
    if (color_depth_.present) {
        const PropertyInfo now =
            lookup_property(fd_, connector_id_, DRM_MODE_OBJECT_CONNECTOR, "color_depth");
        mediabox::logf("color_depth         requested=%s actual=%s (%" PRIu64 ")",
                       state.depth30 ? "30bit" : "Automatic", enum_name_for(now, now.value),
                       now.value);
    }
    {
        // Recorded because "actual" is misleading here and that is the point:
        // color_format is a request channel, not a status channel. What is
        // actually on the wire only shows up in debugfs bus_format.
        const PropertyInfo now =
            lookup_property(fd_, connector_id_, DRM_MODE_OBJECT_CONNECTOR, "color_format");
        if (now.present)
            mediabox::logf("color_format        requested=<not set by probe> actual=%s (%" PRIu64 ")",
                           enum_name_for(now, now.value), now.value);
    }
    if (hdr_metadata_.present) {
        const PropertyInfo now =
            lookup_property(fd_, connector_id_, DRM_MODE_OBJECT_CONNECTOR, "HDR_OUTPUT_METADATA");
        mediabox::logf("HDR_OUTPUT_METADATA requested_blob=%u actual_blob=%" PRIu64,
                       state.hdr_blob_id, now.value);
    }
}

void Display::log_plane_properties() const {
    static const char *kNames[] = {"COLOR_ENCODING", "COLOR_RANGE", "type",
                                   "zpos",           "rotation",   "pixel blend mode"};
    for (const char *name : kNames) {
        const PropertyInfo info = lookup_property(fd_, plane_id_, DRM_MODE_OBJECT_PLANE, name);
        if (!info.present) {
            mediabox::logf("plane %u property %-18s ABSENT", plane_id_, name);
            continue;
        }
        std::string enums;
        for (const auto &e : info.enums) {
            enums += " ";
            enums += e.first;
            enums += "=";
            enums += std::to_string(e.second);
        }
        mediabox::logf("plane %u property %-18s id=%u value=%" PRIu64 "%s (left untouched by this "
                       "gate)",
                       plane_id_, name, info.id, info.value,
                       enums.empty() ? "" : (" enums:" + enums).c_str());
    }
}

void Display::restore_connector_state() {
    if (!saved_connector_state_ || !master_ || !last_fb_id_ || !mode_blob_id_) return;
    drmModeAtomicReq *req = drmModeAtomicAlloc();
    if (!req) return;
    drmModeAtomicAddProperty(req, connector_id_, p_conn_crtc_, crtc_id_);
    drmModeAtomicAddProperty(req, crtc_id_, p_crtc_active_, 1);
    drmModeAtomicAddProperty(req, crtc_id_, p_crtc_mode_, mode_blob_id_);
    if (colorspace_.present)
        drmModeAtomicAddProperty(req, connector_id_, colorspace_.id, saved_colorspace_);
    if (color_depth_.present)
        drmModeAtomicAddProperty(req, connector_id_, color_depth_.id, saved_color_depth_);
    if (hdr_metadata_.present)
        drmModeAtomicAddProperty(req, connector_id_, hdr_metadata_.id, 0);
    drmModeAtomicAddProperty(req, plane_id_, p_plane_fb_, last_fb_id_);
    drmModeAtomicAddProperty(req, plane_id_, p_plane_crtc_, crtc_id_);
    drmModeAtomicAddProperty(req, plane_id_, p_src_x_, static_cast<uint64_t>(last_src_.x) << 16);
    drmModeAtomicAddProperty(req, plane_id_, p_src_y_, static_cast<uint64_t>(last_src_.y) << 16);
    drmModeAtomicAddProperty(req, plane_id_, p_src_w_, static_cast<uint64_t>(last_src_.w) << 16);
    drmModeAtomicAddProperty(req, plane_id_, p_src_h_, static_cast<uint64_t>(last_src_.h) << 16);
    drmModeAtomicAddProperty(req, plane_id_, p_crtc_x_, last_dst_.x);
    drmModeAtomicAddProperty(req, plane_id_, p_crtc_y_, last_dst_.y);
    drmModeAtomicAddProperty(req, plane_id_, p_crtc_w_, last_dst_.w);
    drmModeAtomicAddProperty(req, plane_id_, p_crtc_h_, last_dst_.h);
    const int ret = drmModeAtomicCommit(fd_, req, DRM_MODE_ATOMIC_ALLOW_MODESET, nullptr);
    drmModeAtomicFree(req);
    if (ret) fprintf(stderr, "warning: connector state restore failed: %s\n", strerror(-ret));
    saved_connector_state_ = false;
}

void Display::restore() {
    if (fd_ < 0) return;
    restore_connector_state();
    if (release_framebuffers) {
        release_framebuffers();
        release_framebuffers = nullptr;
    }
    if (hdr_blob_id_) {
        drmModeDestroyPropertyBlob(fd_, hdr_blob_id_);
        hdr_blob_id_ = 0;
    }
    if (mode_blob_id_) {
        drmModeDestroyPropertyBlob(fd_, mode_blob_id_);
        mode_blob_id_ = 0;
    }
    if (master_) {
        drmDropMaster(fd_);
        master_ = false;
    }
    ::close(fd_);
    fd_ = -1;
}

int run_reset(const char *card) {
    std::string error;
    Display d;
    set_restore_target(&d);
    if (!d.open(card, &error)) {
        mediabox::logf("RESET RESULT: BLOCKED (%s)", error.c_str());
        return 1;
    }
    // Reset works on whatever mode is currently live rather than choosing one,
    // so it can be run after any probe without forcing another modeset.
    const int fd = d.fd();
    drmModeRes *res = drmModeGetResources(fd);
    if (!res) {
        mediabox::logf("RESET RESULT: BLOCKED (drmModeGetResources)");
        return 1;
    }
    drmModeConnector *conn = nullptr;
    for (int i = 0; i < res->count_connectors && !conn; ++i) {
        drmModeConnector *c = drmModeGetConnector(fd, res->connectors[i]);
        if (c && c->connector_type == DRM_MODE_CONNECTOR_HDMIA &&
            c->connection == DRM_MODE_CONNECTED && c->encoder_id) {
            conn = c;
            break;
        }
        if (c) drmModeFreeConnector(c);
    }
    if (!conn) {
        mediabox::logf("RESET RESULT: BLOCKED (no active HDMI-A connector)");
        drmModeFreeResources(res);
        return 1;
    }
    drmModeEncoder *encoder = drmModeGetEncoder(fd, conn->encoder_id);
    if (!encoder || !encoder->crtc_id) {
        mediabox::logf("RESET RESULT: BLOCKED (connector %u has no active CRTC)",
                       conn->connector_id);
        if (encoder) drmModeFreeEncoder(encoder);
        drmModeFreeConnector(conn);
        drmModeFreeResources(res);
        return 1;
    }
    const uint32_t connector_id = conn->connector_id;
    const uint32_t crtc_id = encoder->crtc_id;
    drmModeFreeEncoder(encoder);

    drmModeCrtc *crtc = drmModeGetCrtc(fd, crtc_id);
    if (!crtc || !crtc->mode_valid) {
        mediabox::logf("RESET RESULT: BLOCKED (CRTC %u has no valid mode)", crtc_id);
        if (crtc) drmModeFreeCrtc(crtc);
        drmModeFreeConnector(conn);
        drmModeFreeResources(res);
        return 1;
    }
    drmModeModeInfo mode = crtc->mode;
    drmModeFreeCrtc(crtc);

    uint32_t mode_blob = 0;
    if (drmModeCreatePropertyBlob(fd, &mode, sizeof(mode), &mode_blob)) {
        mediabox::logf("RESET RESULT: FAIL (drmModeCreatePropertyBlob: %s)", strerror(errno));
        drmModeFreeConnector(conn);
        drmModeFreeResources(res);
        return 1;
    }

    const PropertyInfo colorspace =
        lookup_property(fd, connector_id, DRM_MODE_OBJECT_CONNECTOR, "Colorspace");
    const PropertyInfo color_depth =
        lookup_property(fd, connector_id, DRM_MODE_OBJECT_CONNECTOR, "color_depth");
    const PropertyInfo hdr =
        lookup_property(fd, connector_id, DRM_MODE_OBJECT_CONNECTOR, "HDR_OUTPUT_METADATA");

    drmModeAtomicReq *req = drmModeAtomicAlloc();
    add_prop(req, connector_id, require_property(fd, connector_id, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID"),
             crtc_id);
    add_prop(req, crtc_id, require_property(fd, crtc_id, DRM_MODE_OBJECT_CRTC, "ACTIVE"), 1);
    add_prop(req, crtc_id, require_property(fd, crtc_id, DRM_MODE_OBJECT_CRTC, "MODE_ID"), mode_blob);
    if (colorspace.present) {
        auto it = colorspace.enums.find("Default");
        if (it != colorspace.enums.end()) add_prop(req, connector_id, colorspace.id, it->second);
    }
    if (color_depth.present) {
        auto it = color_depth.enums.find("Automatic");
        if (it != color_depth.enums.end()) add_prop(req, connector_id, color_depth.id, it->second);
    }
    if (hdr.present) add_prop(req, connector_id, hdr.id, 0);
    const int ret = drmModeAtomicCommit(fd, req, DRM_MODE_ATOMIC_ALLOW_MODESET, nullptr);
    drmModeAtomicFree(req);
    drmModeDestroyPropertyBlob(fd, mode_blob);

    if (ret)
        mediabox::logf("RESET RESULT: FAIL (atomic commit: %s)", strerror(-ret));
    else
        mediabox::logf("reset connector %u to Colorspace=Default color_depth=Automatic "
                       "HDR_OUTPUT_METADATA=unset on mode %s",
                       connector_id, mode.name);
    mediabox::dump_file("/sys/kernel/debug/dri/0/summary", "debugfs summary AFTER reset");
    drmModeFreeConnector(conn);
    drmModeFreeResources(res);
    d.restore();
    set_restore_target(nullptr);
    mediabox::logf("RESET RESULT: %s", ret ? "FAIL" : "PASS");
    return ret ? 1 : 0;
}

}  // namespace mediabox::drm
