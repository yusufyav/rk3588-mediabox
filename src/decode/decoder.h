// Hardware HEVC decode into DRM PRIME frames.
//
// Gate MP1b forbids every software fallback: no libswscale, no CPU colour
// conversion, no 10-bit to 8-bit narrowing, no software HEVC decode. This
// wrapper therefore pins AV_PIX_FMT_DRM_PRIME in get_format and reports what
// the decoder actually produced rather than negotiating anything down.
#pragma once

#include <string>

extern "C" {
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/dovi_meta.h>
#include <libavutil/mastering_display_metadata.h>
}

namespace mediabox::decode {

// Everything a report needs to state about the asset, read from the container
// and the codec context before a single frame is displayed.
struct AssetInfo {
    std::string path;
    std::string container;
    int64_t file_size = 0;

    std::string codec;
    std::string profile;
    int profile_id = 0;
    int level = 0;
    int width = 0;
    int height = 0;
    std::string pix_fmt;
    int bit_depth = 0;
    std::string chroma;

    AVRational avg_frame_rate{0, 1};
    AVRational r_frame_rate{0, 1};
    AVRational time_base{0, 1};
    double duration_seconds = 0.0;
    int64_t bit_rate = 0;
    int64_t nb_frames = 0;

    std::string color_range;
    std::string color_primaries;
    std::string color_transfer;
    std::string color_space;
    std::string chroma_location;

    // Side data presence. Dolby Vision and HDR10+ are recorded because the
    // gate asks for them to be reported, not because anything is done with
    // them: MP1b evaluates the HDR10 base layer only.
    bool has_dovi_config = false;
    std::string dovi_description;
    bool has_hdr10plus = false;
};

class Decoder {
  public:
    Decoder() = default;
    Decoder(const Decoder &) = delete;
    Decoder &operator=(const Decoder &) = delete;
    ~Decoder() { close(); }

    bool open(const char *path, const char *decoder_name, std::string *error);
    void close();

    // 1 = frame produced, 0 = end of stream, -1 = decode error.
    int next_frame(AVFrame *frame, std::string *error);

    // Seeks to `seconds` and flushes the decoder. Used both for --start and to
    // loop a short asset; every seek is counted and reported.
    bool seek_seconds(double seconds, std::string *error);

    const AssetInfo &info() const { return info_; }
    AVCodecContext *codec_context() { return codec_; }
    AVStream *stream() { return fmt_ ? fmt_->streams[stream_index_] : nullptr; }
    long seeks() const { return seeks_; }

    // Converts a stream PTS to seconds from the start of the stream.
    double pts_seconds(int64_t pts) const;

    void log_asset_summary() const;

  private:
    void collect_info(const char *path);

    AVFormatContext *fmt_ = nullptr;
    AVCodecContext *codec_ = nullptr;
    AVBufferRef *hw_device_ = nullptr;
    AVPacket *packet_ = nullptr;
    int stream_index_ = -1;
    long seeks_ = 0;
    bool draining_ = false;
    AssetInfo info_;
};

}  // namespace mediabox::decode
