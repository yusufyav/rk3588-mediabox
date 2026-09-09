// hdr-playback-probe - Gate MP1b real HDR10 content and picture-fidelity probe.
//
// Gate MP1a proved the *signalling* path: given 10-bit NV15 frames, BT.2020
// colorimetry, the vendor color_depth=30bit property and an HDR_OUTPUT_METADATA
// blob, the Sony sink enters HDR10. It proved that with a three-second
// synthetic test pattern, which says nothing about how graded film looks.
//
// This tool answers the one question MP1b asks:
//
//     does that same A4 output state show a real 4K23.976 HEVC Main10 HDR10
//     film with correct colour, tone, highlights and shadows?
//
// It therefore holds the MP1a A4 state fixed -- Colorspace=BT2020_YCC,
// color_depth=30bit, HDR_OUTPUT_METADATA set -- and changes exactly one
// variable: the content. Everything new here is about real content rather than
// about signalling:
//
//   * the output mode is chosen from the asset's own frame rate, and a
//     mismatch is reported rather than silently accepted; 60 Hz fallback is
//     not a pass for this gate,
//   * frames are presented against their PTS instead of as fast as they
//     decode, and cadence is measured from the DRM vblank counter, and
//   * the HDR_OUTPUT_METADATA blob is built from the film's own mastering
//     display metadata, not from anything hard-coded.
//
// Forbidden throughout, and checked rather than assumed: software decode, any
// 10-bit to 8-bit narrowing, NV15 to NV12, BT.2020 to BT.709, PQ to SDR,
// software tone mapping and CPU colour conversion. The probe refuses to
// display a frame that is not DRM PRIME NV15.

#include "common/log.h"
#include "decode/decoder.h"
#include "decode/framebuffer.h"
#include "drm/display.h"
#include "media/cadence.h"
#include "media/hdr_metadata.h"

#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>

#include <unistd.h>

#include <drm_fourcc.h>

extern "C" {
#include <libavutil/pixdesc.h>
}

using mediabox::logf;
using mediabox::monotonic_seconds;

namespace {

constexpr const char *kDebugfsSummary = "/sys/kernel/debug/dri/0/summary";

struct Options {
    const char *card = "/dev/dri/card0";
    const char *input = nullptr;
    const char *decoder = "hevc_rkmpp";
    double duration = 60.0;
    double start = 0.0;
    double refresh_override = 0.0;
    double drop_after = 4.0;
    uint32_t forced_plane = 0;
    bool loop = false;
    bool reset_only = false;
    bool allow_rate_mismatch = false;
    bool debugfs_during = false;
    long warmup_frames = 24;
};

void usage() {
    printf(
        "usage:\n"
        "  hdr-playback-probe --input FILE [options]\n"
        "  hdr-playback-probe --reset\n"
        "\n"
        "options:\n"
        "  --input FILE        real HDR10 asset to play (required)\n"
        "  --duration SECONDS  how long to play (default 60)\n"
        "  --start SECONDS     seek here before playback starts (default 0)\n"
        "  --loop              restart the asset at EOF instead of stopping\n"
        "  --card PATH         DRM card (default /dev/dri/card0)\n"
        "  --decoder NAME      libavcodec decoder (default hevc_rkmpp)\n"
        "  --refresh HZ        override the mode rate taken from the asset\n"
        "  --drop-after N      drop a frame this many refresh intervals late (default 4)\n"
        "  --warmup N          frames shown free-running before the playback clock\n"
        "                      is anchored and measurement starts (default 24)\n"
        "  --debugfs-during    dump the VOP2 summary mid-run from inside the probe;\n"
        "                      costs one frame interval, so it is off by default and\n"
        "                      the runner samples it over ssh instead\n"
        "  --plane ID          restrict the plane search to this plane id\n"
        "  --allow-rate-mismatch  do not treat a wrong output rate as a failure\n"
        "\n"
        "The output state is fixed at the Gate MP1a A4 configuration and is not\n"
        "a variable of this gate: Colorspace=BT2020_YCC, color_depth=30bit,\n"
        "HDR_OUTPUT_METADATA built from the asset. --reset returns those three\n"
        "sticky connector properties to their neutral SDR values.\n");
}

// The plane rectangle. A 1.85:1 film is coded as 3840x2080, not 3840x2160, so
// the common case is a frame that does not fill the mode. It is centred at its
// native size rather than scaled: engaging the VOP2 scaler would put a
// resampler in the path of a gate about picture fidelity, and the letterbox a
// 1.85:1 film wants is exactly what centring produces.
struct Placement {
    mediabox::drm::Rect src;
    mediabox::drm::Rect dst;
    bool scaled = false;
};

Placement place(int frame_w, int frame_h, const drmModeModeInfo &mode) {
    Placement p;
    p.src = {0, 0, static_cast<uint32_t>(frame_w), static_cast<uint32_t>(frame_h)};
    if (frame_w <= mode.hdisplay && frame_h <= mode.vdisplay) {
        p.dst = {static_cast<uint32_t>((mode.hdisplay - frame_w) / 2),
                 static_cast<uint32_t>((mode.vdisplay - frame_h) / 2),
                 static_cast<uint32_t>(frame_w), static_cast<uint32_t>(frame_h)};
        p.scaled = false;
    } else {
        // Larger than the mode: the only alternative to scaling would be to
        // crop the film, which is worse. Reported so the fidelity assessment
        // can account for a resampler being present.
        const double scale = std::min(static_cast<double>(mode.hdisplay) / frame_w,
                                      static_cast<double>(mode.vdisplay) / frame_h);
        const uint32_t w = static_cast<uint32_t>(frame_w * scale);
        const uint32_t h = static_cast<uint32_t>(frame_h * scale);
        p.dst = {(mode.hdisplay - w) / 2, (mode.vdisplay - h) / 2, w, h};
        p.scaled = true;
    }
    return p;
}

int run(const Options &opt) {
    // ---- asset first: nothing touches the display until the file is known to
    // be what this gate requires -------------------------------------------
    mediabox::decode::Decoder decoder;
    std::string error;
    if (!decoder.open(opt.input, opt.decoder, &error)) {
        logf("PLAYBACK RESULT: BLOCKED_ASSET (%s)", error.c_str());
        return 1;
    }
    decoder.log_asset_summary();

    mediabox::media::HdrSource hdr;
    mediabox::media::collect_stream_hdr(decoder.codec_context(), decoder.stream(), &hdr);
    mediabox::media::log_hdr_source(hdr);

    const mediabox::decode::AssetInfo &asset = decoder.info();
    if (asset.has_dovi_config)
        logf("asset NOTE: Dolby Vision configuration present (%s). This gate evaluates the "
             "HDR10 base layer only and outputs no DV metadata.",
             asset.dovi_description.c_str());

    if (hdr.trc != AVCOL_TRC_SMPTE2084) {
        logf("PLAYBACK RESULT: BLOCKED_ASSET (transfer characteristic is %s, not SMPTE ST2084; "
             "this is not HDR10 content)",
             av_color_transfer_name(hdr.trc));
        return 1;
    }
    if (asset.bit_depth != 10) {
        logf("PLAYBACK RESULT: BLOCKED_ASSET (asset is %d-bit; this gate requires 10-bit)",
             asset.bit_depth);
        return 1;
    }

    // ---- output rate follows the asset ------------------------------------
    double target_hz = opt.refresh_override;
    if (target_hz <= 0.0) {
        if (asset.avg_frame_rate.den && asset.avg_frame_rate.num)
            target_hz = av_q2d(asset.avg_frame_rate);
        else if (asset.r_frame_rate.den && asset.r_frame_rate.num)
            target_hz = av_q2d(asset.r_frame_rate);
    }
    if (target_hz <= 0.0) {
        logf("PLAYBACK RESULT: BLOCKED_ASSET (no usable frame rate in the container)");
        return 1;
    }
    logf("playback target_rate=%.4f fps (source: %s) duration=%.1fs start=%.1fs loop=%d",
         target_hz, opt.refresh_override > 0.0 ? "--refresh override" : "asset frame rate",
         opt.duration, opt.start, opt.loop);

    // ---- display ----------------------------------------------------------
    mediabox::drm::Display display;
    mediabox::drm::set_restore_target(&display);
    if (!display.open(opt.card, &error)) {
        logf("PLAYBACK RESULT: BLOCKED_DISPLAY (%s)", error.c_str());
        return 1;
    }
    if (!display.discover(target_hz, &error)) {
        logf("PLAYBACK RESULT: BLOCKED_DISPLAY (%s)", error.c_str());
        return 1;
    }
    const double actual_hz = display.refresh_hz();
    const double rate_error = std::fabs(actual_hz - target_hz);
    bool rate_mismatch = false;
    if (rate_error > 0.01) {
        // A 23.976 film pulled to 60 Hz is the exact failure this gate exists
        // to catch, so the tolerance is tight and the outcome is loud.
        rate_mismatch = true;
        logf("mode MISMATCH: asset wants %.4f fps but the closest available mode is %.4f Hz "
             "(error %.4f Hz)",
             target_hz, actual_hz, rate_error);
    }
    if (!display.has_hdr_metadata_property()) {
        logf("PLAYBACK RESULT: BLOCKED_DISPLAY (connector has no HDR_OUTPUT_METADATA property)");
        return 1;
    }

    // ---- first frame -------------------------------------------------------
    if (opt.start > 0.0) {
        if (!decoder.seek_seconds(opt.start, &error)) {
            logf("PLAYBACK RESULT: BLOCKED_DECODE (seek to %.3fs: %s)", opt.start, error.c_str());
            return 1;
        }
        logf("seek to %.3fs before playback", opt.start);
    }

    AVFrame *frame = av_frame_alloc();
    if (!frame) mediabox::fail("av_frame_alloc");

    mediabox::media::CadenceStats stats;
    int got = decoder.next_frame(frame, &error);
    if (got != 1) {
        logf("PLAYBACK RESULT: BLOCKED_DECODE (no first frame: %s)",
             got == 0 ? "end of stream" : error.c_str());
        av_frame_free(&frame);
        return 1;
    }
    ++stats.decoded;
    if (frame->format != AV_PIX_FMT_DRM_PRIME) {
        logf("PLAYBACK RESULT: BLOCKED_DECODE (decoder returned %s, not drm_prime; this gate "
             "forbids any software decode or CPU conversion)",
             av_get_pix_fmt_name(static_cast<AVPixelFormat>(frame->format)));
        av_frame_free(&frame);
        return 1;
    }
    const AVDRMFrameDescriptor *desc =
        reinterpret_cast<const AVDRMFrameDescriptor *>(frame->data[0]);
    char got_fmt[5];
    mediabox::fourcc_string(desc->layers[0].format, got_fmt);
    logf("decoder drm_prime layer_format=%s modifier=0x%llx objects=%d planes=%d frame=%dx%d",
         got_fmt, static_cast<unsigned long long>(desc->objects[0].format_modifier),
         desc->nb_objects, desc->layers[0].nb_planes, frame->width, frame->height);
    if (desc->layers[0].format != DRM_FORMAT_NV15) {
        logf("PLAYBACK RESULT: BLOCKED_DECODE (decoder produced %s but this gate requires NV15; "
             "NV12 would mean the 10-bit samples were narrowed to 8-bit)",
             got_fmt);
        av_frame_free(&frame);
        return 1;
    }

    // Frame side data is authoritative: it is what the decoder parsed out of
    // this stream, rather than what the container claimed.
    mediabox::media::refine_from_frame(frame, &hdr);
    const bool first_frame_hdr10plus = mediabox::media::frame_has_hdr10plus(frame);
    if (first_frame_hdr10plus)
        logf("asset NOTE: HDR10+ dynamic metadata (SMPTE ST 2094-40) present on frames. This "
             "gate outputs HDR10 static metadata only; dynamic metadata is out of scope.");
    logf("---- HDR metadata as built from this asset ----");
    mediabox::media::log_hdr_source(hdr);

    struct hdr_output_metadata metadata {};
    std::string notes;
    if (!mediabox::media::build_hdr_metadata(hdr, &metadata, &notes)) {
        logf("PLAYBACK RESULT: BLOCKED_ASSET (%s)", notes.c_str());
        av_frame_free(&frame);
        return 1;
    }
    if (!notes.empty()) logf("hdr_metadata provenance notes: %s", notes.c_str());
    mediabox::media::log_hdr_metadata(metadata);

    mediabox::drm::OutputState state;
    state.bt2020_ycc = true;
    state.depth30 = true;
    if (!display.create_hdr_blob(&metadata, sizeof(metadata), &state.hdr_blob_id, &error)) {
        logf("PLAYBACK RESULT: FAIL (%s)", error.c_str());
        av_frame_free(&frame);
        return 1;
    }
    logf("hdr_metadata blob_id=%u", state.hdr_blob_id);

    mediabox::decode::FramebufferCache framebuffers(display.fd());
    static mediabox::decode::FramebufferCache *release_target = &framebuffers;
    display.release_framebuffers = [] {
        if (release_target) release_target->clear();
    };

    uint32_t first_fb = 0;
    framebuffers.retain_in_flight(frame);
    if (!framebuffers.get(desc, frame, &first_fb, &error)) {
        logf("PLAYBACK RESULT: FAIL (framebuffer import: %s)", error.c_str());
        av_frame_free(&frame);
        return 1;
    }

    const Placement placement = place(frame->width, frame->height, display.mode());
    logf("placement src=%ux%u+%u+%u dst=%ux%u+%u+%u scaling=%s", placement.src.w, placement.src.h,
         placement.src.x, placement.src.y, placement.dst.w, placement.dst.h, placement.dst.x,
         placement.dst.y, placement.scaled ? "ENGAGED (VOP2 scaler)" : "none (1:1, centred)");

    if (!display.select_plane(DRM_FORMAT_NV15, opt.forced_plane, state, first_fb, placement.src,
                              placement.dst, &error)) {
        logf("PLAYBACK RESULT: FAIL (%s)", error.c_str());
        av_frame_free(&frame);
        return 1;
    }

    display.log_plane_properties();

    mediabox::dump_file(kDebugfsSummary, "debugfs summary BEFORE playback");

    const int modeset_ret = display.submit(first_fb, true, state, placement.src, placement.dst);
    if (modeset_ret) {
        logf("PLAYBACK RESULT: FAIL (atomic modeset commit: %s)", strerror(-modeset_ret));
        av_frame_free(&frame);
        return 1;
    }
    mediabox::drm::FlipInfo flip;
    if (!display.wait_flip(&flip, 2000)) {
        logf("warning: modeset page-flip event did not arrive within 2000 ms");
        ++stats.flip_timeouts;
    }
    logf("atomic modeset committed");
    mediabox::dump_file(kDebugfsSummary, "debugfs summary AFTER modeset");
    display.report_readback(state);

    // ---- playback ----------------------------------------------------------
    mediabox::media::Pacer pacer(actual_hz, opt.drop_after);
    const double expected_period_ms = 1000.0 / target_hz;
    logf("playback begins: expected frame period %.3f ms at %.4f fps; display refresh %.4f Hz "
         "(vblank %.3f ms)",
         expected_period_ms, target_hz, actual_hz, pacer.vblank_seconds() * 1000.0);

    const double playback_start = monotonic_seconds();
    const double deadline = playback_start + opt.duration;
    const double midpoint = playback_start + opt.duration / 2.0;

    // The first frame is already on screen from the modeset. The playback
    // clock is deliberately NOT anchored on it: the seek, the decoder filling
    // its buffer pool and the USB disk building read-ahead all land in the
    // first second, and anchoring there would bake that startup latency into
    // every later frame as permanent "lateness". Frames run free until the
    // warmup count is reached, then the clock is anchored and measurement
    // starts, so the reported cadence is steady-state cadence.
    if (flip.valid)
        mediabox::media::record_flip(&stats, flip.sequence, flip.timestamp, 0.0,
                                     pacer.vblank_seconds());
    av_frame_unref(frame);

    long decode_errors = 0;
    long hdr10plus_frames = first_frame_hdr10plus ? 1 : 0;
    long metadata_changes = 0;
    long loops = 0;
    bool eof_reached = false;
    bool midpoint_sampled = false;
    double last_progress = playback_start;

    while (monotonic_seconds() < deadline) {
        got = decoder.next_frame(frame, &error);
        if (got == 0) {
            eof_reached = true;
            if (!opt.loop) {
                logf("end of stream after %ld decoded frames", stats.decoded);
                break;
            }
            ++loops;
            logf("end of stream; looping back to %.3fs (loop %ld)", opt.start, loops);
            if (!decoder.seek_seconds(opt.start, &error)) {
                logf("loop seek failed: %s", error.c_str());
                break;
            }
            // A loop restarts the presentation timeline: PTS jumps backwards,
            // so the anchor has to move with it or every frame after the loop
            // would look absurdly late and be dropped.
            continue;
        }
        if (got < 0) {
            ++decode_errors;
            logf("decode error: %s", error.c_str());
            break;
        }
        ++stats.decoded;

        if (frame->format != AV_PIX_FMT_DRM_PRIME) {
            ++decode_errors;
            logf("decoder returned %s mid-run, not drm_prime; stopping rather than converting",
                 av_get_pix_fmt_name(static_cast<AVPixelFormat>(frame->format)));
            break;
        }
        const AVDRMFrameDescriptor *fd_desc =
            reinterpret_cast<const AVDRMFrameDescriptor *>(frame->data[0]);
        if (fd_desc->layers[0].format != DRM_FORMAT_NV15) {
            ++decode_errors;
            logf("decoder changed frame format mid-run; stopping rather than converting");
            break;
        }

        // Static metadata is constant across a normal HDR10 stream. A change
        // is logged rather than acted on: re-signalling mid-stream is out of
        // scope for this gate.
        mediabox::media::HdrSource probe_hdr = hdr;
        if (mediabox::media::refine_from_frame(frame, &probe_hdr)) {
            ++metadata_changes;
            if (metadata_changes <= 5) {
                logf("NOTE: stream HDR static metadata changed at decoded frame %ld; the "
                     "HDR_OUTPUT_METADATA blob is NOT being updated (out of scope)",
                     stats.decoded);
                mediabox::media::log_hdr_source(probe_hdr);
            }
            hdr = probe_hdr;
        }
        if (mediabox::media::frame_has_hdr10plus(frame)) ++hdr10plus_frames;

        const int64_t ts = frame->best_effort_timestamp != AV_NOPTS_VALUE
                               ? frame->best_effort_timestamp
                               : frame->pts;
        const bool have_pts = (ts != AV_NOPTS_VALUE);
        if (!have_pts) ++stats.no_pts;
        const double pts = have_pts ? decoder.pts_seconds(ts) : 0.0;

        if (have_pts && !pacer.anchored() && stats.presented >= opt.warmup_frames) {
            // Anchor on where this frame's flip is expected to land -- one
            // refresh interval after the last one -- rather than on "now",
            // which would be somewhere inside the current interval.
            pacer.anchor(pts, stats.last_flip_time + pacer.vblank_seconds());
            logf("playback clock anchored after %ld warmup frames at pts %.3fs; steady-state "
                 "cadence measurement starts here",
                 stats.presented, pts);
            mediabox::media::start_steady_state(&stats);
        }

        double target = 0.0;
        if (have_pts && pacer.anchored()) {
            target = pacer.target_for(pts);
            const double now = monotonic_seconds();
            // After a loop the PTS runs backwards; re-anchor instead of
            // dropping the whole next pass as "late".
            if (loops > 0 && target < now - 1.0) {
                pacer.anchor(pts, now);
                target = pacer.target_for(pts);
                logf("playback clock re-anchored after loop at pts %.3fs", pts);
            }
            if (pacer.should_drop(target, now)) {
                ++stats.dropped;
                av_frame_unref(frame);
                continue;
            }
            pacer.wait_for(target);
        }

        uint32_t fb_id = 0;
        if (!framebuffers.get(fd_desc, frame, &fb_id, &error)) {
            logf("framebuffer import failed mid-run: %s", error.c_str());
            ++stats.flip_errors;
            break;
        }
        framebuffers.retain_in_flight(frame);

        const int ret = display.submit(fb_id, false, state, placement.src, placement.dst);
        if (ret) {
            logf("atomic page-flip commit failed: %s", strerror(-ret));
            ++stats.flip_errors;
            break;
        }
        if (display.wait_flip(&flip, 2000)) {
            mediabox::media::record_flip(&stats, flip.sequence, flip.timestamp, target,
                                         pacer.vblank_seconds());
        } else {
            ++stats.flip_timeouts;
            logf("warning: page-flip event wait timed out at decoded frame %ld", stats.decoded);
        }
        av_frame_unref(frame);

        const double now = monotonic_seconds();
        if (opt.debugfs_during && !midpoint_sampled && now >= midpoint) {
            // Reading the VOP2 summary walks the whole display pipeline under
            // the driver's locks and reliably costs a frame interval, so it is
            // opt-in: leaving it out keeps the cadence measurement clean, and
            // the runner samples the same file over ssh instead.
            midpoint_sampled = true;
            logf("---- steady-state sample at t=%.1fs, presented=%ld ----", now - playback_start,
                 stats.presented);
            mediabox::dump_file(kDebugfsSummary, "debugfs summary DURING playback");
        }
        if (now - last_progress >= 10.0) {
            last_progress = now;
            logf("progress t=%.1fs decoded=%ld presented=%ld dropped=%ld repeated=%ld late=%ld "
                 "imported_fbs=%zu",
                 now - playback_start, stats.decoded, stats.presented, stats.dropped,
                 stats.repeated, stats.late, framebuffers.size());
        }
    }

    const double playback_wall = monotonic_seconds() - playback_start;
    logf("---- playback finished after %.3fs wall ----", playback_wall);
    mediabox::dump_file(kDebugfsSummary, "debugfs summary AFTER playback");
    display.report_readback(state);

    mediabox::media::log_cadence(stats, expected_period_ms);
    logf("run decode_errors=%ld seeks=%ld loops=%ld eof=%d hdr10plus_frames=%ld "
         "metadata_changes=%ld imported_framebuffers=%zu",
         decode_errors, decoder.seeks(), loops, eof_reached ? 1 : 0, hdr10plus_frames,
         metadata_changes, framebuffers.size());

    // ---- verdict -----------------------------------------------------------
    // The verdict covers only what a program can see. Colour, tone, highlight
    // and shadow behaviour are assessed on the physical panel and reported
    // separately; nothing here claims to have judged the picture.
    const char *result = "PASS";
    if (stats.presented == 0) result = "BLOCKED_DECODE";
    else if (decode_errors) result = "BLOCKED_DECODE";
    else if (stats.flip_errors) result = "FAIL";
    else if (rate_mismatch && !opt.allow_rate_mismatch) result = "PARTIAL";
    else if (placement.scaled) result = "PARTIAL";
    else if (stats.flip_timeouts) result = "PARTIAL";
    else if (playback_wall < opt.duration * 0.9 && !eof_reached) result = "PARTIAL";

    const double repeat_ratio =
        stats.presented ? static_cast<double>(stats.repeated) / stats.presented : 0.0;
    const double drop_ratio =
        stats.decoded ? static_cast<double>(stats.dropped) / stats.decoded : 0.0;
    logf("cadence_quality repeated_ratio=%.4f dropped_ratio=%.4f", repeat_ratio, drop_ratio);
    if (!strcmp(result, "PASS") && (repeat_ratio > 0.01 || drop_ratio > 0.01)) result = "PARTIAL";

    logf("PLAYBACK RESULT: %s", result);
    logf("PLAYBACK NOTE: this verdict covers pipeline behaviour only. Colour, tone, highlight "
         "and shadow fidelity are assessed on the physical sink and reported separately.");

    av_frame_free(&frame);
    display.restore();
    mediabox::drm::set_restore_target(nullptr);
    release_target = nullptr;
    return strcmp(result, "PASS") == 0 ? 0 : 1;
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
        if (!strcmp(arg, "--input")) opt.input = next();
        else if (!strcmp(arg, "--duration")) opt.duration = atof(next());
        else if (!strcmp(arg, "--start")) opt.start = atof(next());
        else if (!strcmp(arg, "--loop")) opt.loop = true;
        else if (!strcmp(arg, "--card")) opt.card = next();
        else if (!strcmp(arg, "--decoder")) opt.decoder = next();
        else if (!strcmp(arg, "--refresh")) opt.refresh_override = atof(next());
        else if (!strcmp(arg, "--drop-after")) opt.drop_after = atof(next());
        else if (!strcmp(arg, "--plane")) opt.forced_plane = strtoul(next(), nullptr, 0);
        else if (!strcmp(arg, "--allow-rate-mismatch")) opt.allow_rate_mismatch = true;
        else if (!strcmp(arg, "--debugfs-during")) opt.debugfs_during = true;
        else if (!strcmp(arg, "--warmup")) opt.warmup_frames = atol(next());
        else if (!strcmp(arg, "--reset")) opt.reset_only = true;
        else if (!strcmp(arg, "--help") || !strcmp(arg, "-h")) {
            usage();
            return 0;
        } else {
            fprintf(stderr, "error: unknown argument %s\n", arg);
            usage();
            return 2;
        }
    }

    mediabox::drm::install_signal_handlers();
    if (opt.reset_only) {
        alarm(60);
        return mediabox::drm::run_reset(opt.card);
    }
    if (!opt.input) {
        usage();
        return 2;
    }
    // Watchdog: a probe that wedges while holding DRM master leaves the
    // console with no display, so it must never be able to outlive its budget.
    alarm(static_cast<unsigned>(opt.duration) + 150u);
    const int rc = run(opt);
    alarm(0);
    return rc;
}
