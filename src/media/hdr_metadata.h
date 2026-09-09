// Maps a stream's own HDR10 metadata onto the CTA-861.3 Static Metadata
// Descriptor Type 1 payload that HDR_OUTPUT_METADATA carries.
//
// The rule this module exists to enforce: nothing is invented. Fields the
// asset does not carry are left zero, which is the CTA-861.3 encoding for
// "unspecified", and the omission is reported rather than papered over with a
// plausible default.
#pragma once

#include <string>

extern "C" {
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/frame.h>
#include <libavutil/mastering_display_metadata.h>
#include <libavutil/pixdesc.h>
}

#include <drm_mode.h>

namespace mediabox::media {

// The HDR characteristics gathered from an asset, from the coded side data
// and then refined by the first decoded frame's side data, which is
// authoritative because it is what the decoder actually parsed.
struct HdrSource {
    bool have_mastering = false;
    bool have_light_level = false;
    AVMasteringDisplayMetadata mastering{};
    AVContentLightMetadata light{};
    enum AVColorTransferCharacteristic trc = AVCOL_TRC_UNSPECIFIED;
    enum AVColorPrimaries primaries = AVCOL_PRI_UNSPECIFIED;
    enum AVColorSpace space = AVCOL_SPC_UNSPECIFIED;

    bool operator==(const HdrSource &other) const;
    bool operator!=(const HdrSource &other) const { return !(*this == other); }
};

// Reads coded (container/stream level) side data.
void collect_stream_hdr(const AVCodecContext *codec, const AVStream *stream, HdrSource *out);

// Refines from a decoded frame's side data. Returns true if anything changed.
bool refine_from_frame(const AVFrame *frame, HdrSource *inout);

// True if this frame carries HDR10+ dynamic metadata. MP1b reports its
// presence and deliberately does not output it: dynamic metadata is out of
// scope for this gate.
bool frame_has_hdr10plus(const AVFrame *frame);

// Builds the infoframe payload. Returns false, with a reason in `notes`, when
// the stream is not PQ: signalling HDR10 for non-PQ content would be a lie.
bool build_hdr_metadata(const HdrSource &src, struct hdr_output_metadata *out, std::string *notes);

// Logs the payload field by field and as raw bytes, so a report can decode it
// independently of this program.
void log_hdr_metadata(const struct hdr_output_metadata &m);

// Logs the mapping from asset values to infoframe values, including which
// fields the asset did not supply.
void log_hdr_source(const HdrSource &src);

}  // namespace mediabox::media
