// Atomic-KMS output for the mediabox probes.
//
// Provenance: the discovery, atomic-commit and property-restore structure is
// derived from tools/hdr-signaling-probe.cpp (Gate MP1a), which in turn came
// from yusufyav/rk3588-screenbridge tools/hdmirx-direct-display.cpp. It is
// split out here so that Gate MP1b's playback probe can reuse the exact
// output path MP1a validated without editing the MP1a tool, whose source
// hash is published in the MP1a report.
//
// Two things about this platform drive the shape of this interface:
//
//   * there is no standard "max bpc" connector property; the vendor exposes a
//     "color_depth" enum instead, so bit depth is requested by name, and
//   * the only plane on vp0 that accepts NV12/NV15 is typed Cursor, so planes
//     are chosen by atomic TEST_ONLY commit rather than by plane type.
#pragma once

#include <cstdint>
#include <map>
#include <string>
#include <vector>

#include <xf86drm.h>
#include <xf86drmMode.h>

namespace mediabox::drm {

struct PropertyInfo {
    uint32_t id = 0;
    uint64_t value = 0;
    bool present = false;
    std::map<std::string, uint64_t> enums;
};

PropertyInfo lookup_property(int fd, uint32_t object, uint32_t type, const char *name);
uint32_t require_property(int fd, uint32_t object, uint32_t type, const char *name);
const char *enum_name_for(const PropertyInfo &info, uint64_t value);
double mode_refresh(const drmModeModeInfo &m);

// The connector colour state under test. Gate MP1b holds this fixed at the
// values Gate MP1a proved: BT2020_YCC + 30bit + a real HDR10 blob.
struct OutputState {
    bool bt2020_ycc = false;
    bool depth30 = false;
    uint32_t hdr_blob_id = 0;  // 0 means HDR_OUTPUT_METADATA is left unset

    // Gate MP1b-CSC's single variable: the DRM enum name to request on the
    // scanout plane's COLOR_ENCODING property, e.g. "ITU-R BT.2020 YCbCr".
    // nullptr -- the default -- leaves the property untouched, which is
    // exactly the MP1b behaviour whose evidence is already published, so the
    // A leg of the A/B is byte-for-byte the old path. COLOR_RANGE is
    // deliberately absent: changing two colour properties at once would make
    // the comparison two-variable and uninterpretable.
    const char *plane_color_encoding = nullptr;
};

// Source and destination rectangles for the scanout plane. Kept explicit so a
// run can record whether any scaling was engaged; MP1b expects none.
struct Rect {
    uint32_t x = 0, y = 0, w = 0, h = 0;
};

// What the kernel reported about a completed page flip. `sequence` is the
// vblank counter, and it is the authoritative cadence instrument: a delta of
// one means the previous frame was shown for exactly one refresh interval.
struct FlipInfo {
    unsigned int sequence = 0;
    double timestamp = 0.0;  // CLOCK_MONOTONIC seconds
    bool valid = false;
};

class Display {
  public:
    Display() = default;
    Display(const Display &) = delete;
    Display &operator=(const Display &) = delete;
    ~Display() { restore(); }

    // Opens the card and takes DRM master. Fails cleanly if another client
    // (a compositor, or a previous probe that has not exited) holds it.
    bool open(const char *card, std::string *error);

    // Finds the connected HDMI-A connector, its CRTC, and the 3840x2160
    // progressive mode whose refresh is closest to target_hz. Records the
    // connector's current colour properties so they can be put back.
    bool discover(double target_hz, std::string *error);

    // Runs a full atomic TEST_ONLY commit for each plane that can drive this
    // CRTC and accepts `fourcc`, taking the first the kernel accepts. Plane
    // type is deliberately not used as a filter.
    bool select_plane(uint32_t fourcc, uint32_t forced_plane, const OutputState &state,
                      uint32_t first_fb, const Rect &src, const Rect &dst, std::string *error);

    // Runs one atomic TEST_ONLY commit for the already-selected plane and the
    // given state, changing nothing. Returns the kernel's return value (0, or
    // a negative errno). Used to prove a property combination is accepted
    // before a real modeset is attempted with it.
    int test_only(const OutputState &state, uint32_t fb_id, const Rect &src, const Rect &dst) const;

    // Queues an atomic commit with DRM_MODE_PAGE_FLIP_EVENT. Returns 0 on
    // success; the flip has not completed yet on return.
    int submit(uint32_t fb_id, bool modeset, const OutputState &state, const Rect &src,
               const Rect &dst);

    // Blocks until the queued flip completes. Returns false on timeout, which
    // is reported rather than retried: a missed flip event is data.
    bool wait_flip(FlipInfo *out, int timeout_ms);

    // Reads the connector properties back from the kernel after a commit, so
    // that "requested" and "actual" can be reported as separate facts.
    void report_readback(const OutputState &state) const;

    // Inventory of the scanout plane's colour properties, with their enum
    // maps, as found before any commit. Gate MP1a left COLOR_ENCODING and
    // COLOR_RANGE at their defaults and recorded that VOP2 then tags the
    // window SDR/BT.601 while the video port runs HDR10/BT.2020. `state` is
    // passed only so the log can say which of these the run is requesting.
    void log_plane_properties(const OutputState &state) const;

    // True when the plane exposes COLOR_ENCODING and that property has an
    // enum with this name. Only meaningful after select_plane() has bound a
    // plane. A run that asks for an encoding this plane cannot express must
    // report that rather than silently fall back to the default.
    bool plane_supports_color_encoding(const char *enum_name) const;

    // The return value of the last atomic TEST_ONLY commit select_plane()
    // issued, so a caller can report the kernel's own errno for a rejected
    // property combination instead of inventing a reason.
    int last_plane_test_result() const { return last_plane_test_; }

    // Restores the connector colour properties as found, releases blobs and
    // drops master. Safe to call twice; also called from the signal handler.
    void restore();

    int fd() const { return fd_; }
    uint32_t connector_id() const { return connector_id_; }
    uint32_t crtc_id() const { return crtc_id_; }
    uint32_t plane_id() const { return plane_id_; }
    const char *plane_type_name() const { return plane_type_name_.c_str(); }
    const drmModeModeInfo &mode() const { return mode_; }
    double refresh_hz() const { return mode_refresh(mode_); }
    bool has_hdr_metadata_property() const { return hdr_metadata_.present; }

    // Creates a HDR_OUTPUT_METADATA blob owned by this Display.
    bool create_hdr_blob(const void *payload, size_t size, uint32_t *blob_id, std::string *error);

    // Hook the owner sets so that imported framebuffers, which reference this
    // DRM fd, are torn down before it closes -- including on the signal path.
    void (*release_framebuffers)() = nullptr;

  private:
    void build_request(drmModeAtomicReq *req, const OutputState &state, uint32_t fb_id,
                       bool modeset, const Rect &src, const Rect &dst) const;
    void bind_plane_properties();
    void restore_connector_state();

    int fd_ = -1;
    bool master_ = false;

    uint32_t connector_id_ = 0;
    uint32_t crtc_id_ = 0;
    int crtc_index_ = -1;
    uint32_t plane_id_ = 0;
    std::string plane_type_name_ = "?";

    drmModeModeInfo mode_{};
    uint32_t mode_blob_id_ = 0;
    uint32_t hdr_blob_id_ = 0;
    uint32_t last_fb_id_ = 0;
    Rect last_src_{}, last_dst_{};
    OutputState last_state_{};

    // Connector colour values as found before this run. The kernel fbdev
    // client restores its own mode when a master leaves but never touches
    // these, so a probe that just exited would strand the link in
    // BT.2020/10-bit/HDR10 underneath an SDR console.
    bool saved_connector_state_ = false;
    uint64_t saved_colorspace_ = 0;
    uint64_t saved_color_depth_ = 0;

    uint32_t p_conn_crtc_ = 0;
    uint32_t p_crtc_active_ = 0;
    uint32_t p_crtc_mode_ = 0;
    uint32_t p_plane_fb_ = 0, p_plane_crtc_ = 0;
    uint32_t p_src_x_ = 0, p_src_y_ = 0, p_src_w_ = 0, p_src_h_ = 0;
    uint32_t p_crtc_x_ = 0, p_crtc_y_ = 0, p_crtc_w_ = 0, p_crtc_h_ = 0;

    PropertyInfo colorspace_, color_depth_, hdr_metadata_;

    // The scanout plane's colour properties, plus COLOR_ENCODING as it was
    // found. Atomic plane state is sticky the same way the connector's is, so
    // a run that requests BT.2020 here would leave the next run's "default"
    // leg sitting on BT.2020 and quietly destroy the A/B it is half of.
    PropertyInfo plane_color_encoding_, plane_color_range_;
    uint64_t saved_plane_color_encoding_ = 0;
    bool saved_plane_color_encoding_valid_ = false;
    int last_plane_test_ = 0;

    // Planes the previous master (the kernel fbdev client) left bound to our
    // CRTC. They are disabled on modeset: VOP2 blends per window and tracks an
    // HDR/SDR flag per window, so a leftover SDR RGB console window would sit
    // in the same blending path this gate is trying to characterise.
    struct StalePlane {
        uint32_t id;
        uint32_t fb_prop;
        uint32_t crtc_prop;
    };
    std::vector<StalePlane> stale_planes_;

    struct FlipWait {
        bool pending = false;
        FlipInfo info;
    } flip_;

    friend void page_flip_trampoline(int, unsigned int, unsigned int, unsigned int, void *);
};

// Registers `display` as the object the signal handlers and the atexit path
// will tear down. Passing nullptr clears it.
void set_restore_target(Display *display);
void install_signal_handlers();

// Returns the connector to Colorspace=Default, color_depth=Automatic and
// HDR_OUTPUT_METADATA unset on whatever mode is currently live. These
// properties are sticky and no console client clears them.
int run_reset(const char *card);

}  // namespace mediabox::drm
