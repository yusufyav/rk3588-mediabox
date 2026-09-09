#include "decode/decoder.h"

#include "common/log.h"

#include <cinttypes>
#include <cstring>
#include <sys/stat.h>

extern "C" {
#include <libavutil/hwcontext.h>
#include <libavutil/pixdesc.h>
}

namespace mediabox::decode {
namespace {

const char *or_unknown(const char *s) { return s ? s : "unknown"; }

enum AVPixelFormat pick_drm_prime(AVCodecContext *ctx, const enum AVPixelFormat *formats) {
    (void)ctx;
    for (const enum AVPixelFormat *p = formats; *p != AV_PIX_FMT_NONE; ++p)
        if (*p == AV_PIX_FMT_DRM_PRIME) return AV_PIX_FMT_DRM_PRIME;
    // Returning formats[0] here would let libavcodec fall back to software
    // output. The caller checks frame->format and refuses to display anything
    // that is not DRM PRIME, so the fallback is reported rather than used.
    return formats[0];
}

}  // namespace

bool Decoder::open(const char *path, const char *decoder_name, std::string *error) {
    close();
    if (avformat_open_input(&fmt_, path, nullptr, nullptr) < 0) {
        *error = std::string("avformat_open_input failed for ") + path;
        return false;
    }
    if (avformat_find_stream_info(fmt_, nullptr) < 0) {
        *error = "avformat_find_stream_info failed";
        return false;
    }
    stream_index_ = av_find_best_stream(fmt_, AVMEDIA_TYPE_VIDEO, -1, -1, nullptr, 0);
    if (stream_index_ < 0) {
        *error = "no video stream in the container";
        return false;
    }
    const AVCodec *codec = avcodec_find_decoder_by_name(decoder_name);
    if (!codec) {
        *error = std::string("decoder not built into this FFmpeg: ") + decoder_name;
        return false;
    }
    codec_ = avcodec_alloc_context3(codec);
    if (!codec_) {
        *error = "avcodec_alloc_context3 failed";
        return false;
    }
    if (avcodec_parameters_to_context(codec_, fmt_->streams[stream_index_]->codecpar) < 0) {
        *error = "avcodec_parameters_to_context failed";
        return false;
    }
    codec_->get_format = pick_drm_prime;

    // RKMPP needs a hardware device context before it will hand out DRM PRIME
    // frames. The DRM device is the documented fallback on builds where the
    // RKMPP device type is not registered.
    if (av_hwdevice_ctx_create(&hw_device_, AV_HWDEVICE_TYPE_RKMPP, nullptr, nullptr, 0) >= 0) {
        codec_->hw_device_ctx = av_buffer_ref(hw_device_);
    } else if (av_hwdevice_ctx_create(&hw_device_, AV_HWDEVICE_TYPE_DRM, "/dev/dri/card0", nullptr,
                                      0) >= 0) {
        codec_->hw_device_ctx = av_buffer_ref(hw_device_);
    }

    if (avcodec_open2(codec_, codec, nullptr) < 0) {
        *error = "avcodec_open2 failed";
        return false;
    }
    packet_ = av_packet_alloc();
    if (!packet_) {
        *error = "av_packet_alloc failed";
        return false;
    }
    collect_info(path);
    return true;
}

void Decoder::collect_info(const char *path) {
    AVStream *st = fmt_->streams[stream_index_];
    info_ = AssetInfo{};
    info_.path = path;
    info_.container = or_unknown(fmt_->iformat ? fmt_->iformat->name : nullptr);
    struct stat sb {};
    if (stat(path, &sb) == 0) info_.file_size = sb.st_size;

    info_.codec = or_unknown(avcodec_get_name(codec_->codec_id));
    info_.profile_id = codec_->profile;
    info_.profile = or_unknown(avcodec_profile_name(codec_->codec_id, codec_->profile));
    info_.level = codec_->level;
    info_.width = codec_->width;
    info_.height = codec_->height;

    // With a hardware decoder the context pix_fmt is the DRM PRIME wrapper, so
    // the meaningful software format is the one the stream is coded in.
    const AVPixelFormat coded = static_cast<AVPixelFormat>(st->codecpar->format);
    info_.pix_fmt = or_unknown(av_get_pix_fmt_name(coded));
    if (const AVPixFmtDescriptor *d = av_pix_fmt_desc_get(coded)) {
        info_.bit_depth = d->comp[0].depth;
        if (d->log2_chroma_w == 1 && d->log2_chroma_h == 1) info_.chroma = "4:2:0";
        else if (d->log2_chroma_w == 1 && d->log2_chroma_h == 0) info_.chroma = "4:2:2";
        else if (d->log2_chroma_w == 0 && d->log2_chroma_h == 0) info_.chroma = "4:4:4";
        else info_.chroma = "other";
    }

    info_.avg_frame_rate = st->avg_frame_rate;
    info_.r_frame_rate = st->r_frame_rate;
    info_.time_base = st->time_base;
    info_.nb_frames = st->nb_frames;
    if (st->duration != AV_NOPTS_VALUE && st->time_base.den)
        info_.duration_seconds = static_cast<double>(st->duration) * av_q2d(st->time_base);
    else if (fmt_->duration != AV_NOPTS_VALUE)
        info_.duration_seconds = static_cast<double>(fmt_->duration) / AV_TIME_BASE;
    info_.bit_rate = st->codecpar->bit_rate ? st->codecpar->bit_rate : fmt_->bit_rate;

    info_.color_range = or_unknown(av_color_range_name(codec_->color_range));
    info_.color_primaries = or_unknown(av_color_primaries_name(codec_->color_primaries));
    info_.color_transfer = or_unknown(av_color_transfer_name(codec_->color_trc));
    info_.color_space = or_unknown(av_color_space_name(codec_->colorspace));
    info_.chroma_location = or_unknown(av_chroma_location_name(codec_->chroma_sample_location));

    for (int i = 0; i < st->codecpar->nb_coded_side_data; ++i) {
        const AVPacketSideData &sd = st->codecpar->coded_side_data[i];
        if (sd.type == AV_PKT_DATA_DOVI_CONF && sd.size >= sizeof(AVDOVIDecoderConfigurationRecord)) {
            const auto *dovi = reinterpret_cast<const AVDOVIDecoderConfigurationRecord *>(sd.data);
            info_.has_dovi_config = true;
            char buf[160];
            snprintf(buf, sizeof(buf),
                     "version=%u.%u profile=%u level=%u rpu_present=%u el_present=%u bl_present=%u "
                     "bl_signal_compatibility_id=%u",
                     dovi->dv_version_major, dovi->dv_version_minor, dovi->dv_profile,
                     dovi->dv_level, dovi->rpu_present_flag, dovi->el_present_flag,
                     dovi->bl_present_flag, dovi->dv_bl_signal_compatibility_id);
            info_.dovi_description = buf;
        }
    }
}

void Decoder::log_asset_summary() const {
    const AssetInfo &a = info_;
    mediabox::logf("asset path=%s size=%" PRId64 " bytes container=%s", a.path.c_str(),
                   a.file_size, a.container.c_str());
    mediabox::logf("asset codec=%s profile=%s(%d) level=%d %dx%d pix_fmt=%s bit_depth=%d chroma=%s",
                   a.codec.c_str(), a.profile.c_str(), a.profile_id, a.level, a.width, a.height,
                   a.pix_fmt.c_str(), a.bit_depth, a.chroma.c_str());
    mediabox::logf("asset avg_frame_rate=%d/%d (%.6f) r_frame_rate=%d/%d time_base=%d/%d "
                   "duration=%.3fs nb_frames=%" PRId64 " bit_rate=%" PRId64,
                   a.avg_frame_rate.num, a.avg_frame_rate.den,
                   a.avg_frame_rate.den ? av_q2d(a.avg_frame_rate) : 0.0, a.r_frame_rate.num,
                   a.r_frame_rate.den, a.time_base.num, a.time_base.den, a.duration_seconds,
                   a.nb_frames, a.bit_rate);
    mediabox::logf("asset color_range=%s primaries=%s transfer=%s space=%s chroma_location=%s",
                   a.color_range.c_str(), a.color_primaries.c_str(), a.color_transfer.c_str(),
                   a.color_space.c_str(), a.chroma_location.c_str());
    if (a.has_dovi_config)
        mediabox::logf("asset dolby_vision_configuration present: %s", a.dovi_description.c_str());
    else
        mediabox::logf("asset dolby_vision_configuration absent");
}

double Decoder::pts_seconds(int64_t pts) const {
    if (pts == AV_NOPTS_VALUE || !fmt_) return 0.0;
    return static_cast<double>(pts) * av_q2d(fmt_->streams[stream_index_]->time_base);
}

int Decoder::next_frame(AVFrame *frame, std::string *error) {
    for (;;) {
        const int ret = avcodec_receive_frame(codec_, frame);
        if (ret == 0) return 1;
        if (ret == AVERROR_EOF) return 0;
        if (ret != AVERROR(EAGAIN)) {
            char buf[128];
            av_strerror(ret, buf, sizeof(buf));
            *error = std::string("avcodec_receive_frame: ") + buf;
            return -1;
        }
        if (draining_) return 0;
        const int read = av_read_frame(fmt_, packet_);
        if (read < 0) {
            // End of container: flush the decoder so buffered frames come out.
            draining_ = true;
            avcodec_send_packet(codec_, nullptr);
            continue;
        }
        if (packet_->stream_index != stream_index_) {
            av_packet_unref(packet_);
            continue;
        }
        const int sent = avcodec_send_packet(codec_, packet_);
        av_packet_unref(packet_);
        if (sent < 0 && sent != AVERROR(EAGAIN)) {
            char buf[128];
            av_strerror(sent, buf, sizeof(buf));
            *error = std::string("avcodec_send_packet: ") + buf;
            return -1;
        }
    }
}

bool Decoder::seek_seconds(double seconds, std::string *error) {
    AVStream *st = fmt_->streams[stream_index_];
    const int64_t target = static_cast<int64_t>(seconds / av_q2d(st->time_base));
    if (av_seek_frame(fmt_, stream_index_, target, AVSEEK_FLAG_BACKWARD) < 0) {
        *error = "av_seek_frame failed";
        return false;
    }
    avcodec_flush_buffers(codec_);
    draining_ = false;
    ++seeks_;
    return true;
}

void Decoder::close() {
    if (packet_) av_packet_free(&packet_);
    if (codec_) avcodec_free_context(&codec_);
    if (fmt_) avformat_close_input(&fmt_);
    if (hw_device_) av_buffer_unref(&hw_device_);
    stream_index_ = -1;
    draining_ = false;
}

}  // namespace mediabox::decode
