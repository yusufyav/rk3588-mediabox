#include "media/hdr_metadata.h"

#include "common/log.h"

#include <cmath>
#include <cstring>
#include <string>

namespace mediabox::media {
namespace {

// CTA-861.3 codes chromaticity in units of 0.00002.
uint16_t to_primary_units(AVRational value) {
    const double v = av_q2d(value) * 50000.0;
    if (v < 0.0) return 0;
    if (v > 65535.0) return 65535;
    return static_cast<uint16_t>(std::lround(v));
}

// Max mastering luminance is coded in units of 1 cd/m2.
uint16_t to_max_luminance_units(AVRational value) {
    const double v = av_q2d(value);
    if (v < 0.0) return 0;
    if (v > 65535.0) return 65535;
    return static_cast<uint16_t>(std::lround(v));
}

// Min mastering luminance is coded in units of 0.0001 cd/m2.
uint16_t to_min_luminance_units(AVRational value) {
    const double v = av_q2d(value) * 10000.0;
    if (v < 0.0) return 0;
    if (v > 65535.0) return 65535;
    return static_cast<uint16_t>(std::lround(v));
}

bool same_rational(AVRational a, AVRational b) { return a.num == b.num && a.den == b.den; }

}  // namespace

bool HdrSource::operator==(const HdrSource &o) const {
    if (have_mastering != o.have_mastering || have_light_level != o.have_light_level ||
        trc != o.trc || primaries != o.primaries || space != o.space)
        return false;
    if (have_mastering) {
        if (mastering.has_primaries != o.mastering.has_primaries ||
            mastering.has_luminance != o.mastering.has_luminance)
            return false;
        if (mastering.has_primaries) {
            for (int i = 0; i < 3; ++i)
                for (int j = 0; j < 2; ++j)
                    if (!same_rational(mastering.display_primaries[i][j],
                                       o.mastering.display_primaries[i][j]))
                        return false;
            for (int j = 0; j < 2; ++j)
                if (!same_rational(mastering.white_point[j], o.mastering.white_point[j]))
                    return false;
        }
        if (mastering.has_luminance &&
            (!same_rational(mastering.min_luminance, o.mastering.min_luminance) ||
             !same_rational(mastering.max_luminance, o.mastering.max_luminance)))
            return false;
    }
    if (have_light_level &&
        (light.MaxCLL != o.light.MaxCLL || light.MaxFALL != o.light.MaxFALL))
        return false;
    return true;
}

void collect_stream_hdr(const AVCodecContext *codec, const AVStream *stream, HdrSource *out) {
    out->trc = codec->color_trc;
    out->primaries = codec->color_primaries;
    out->space = codec->colorspace;
    for (int i = 0; i < stream->codecpar->nb_coded_side_data; ++i) {
        const AVPacketSideData &sd = stream->codecpar->coded_side_data[i];
        if (sd.type == AV_PKT_DATA_MASTERING_DISPLAY_METADATA &&
            sd.size >= sizeof(AVMasteringDisplayMetadata)) {
            out->mastering = *reinterpret_cast<const AVMasteringDisplayMetadata *>(sd.data);
            out->have_mastering = true;
        } else if (sd.type == AV_PKT_DATA_CONTENT_LIGHT_LEVEL &&
                   sd.size >= sizeof(AVContentLightMetadata)) {
            out->light = *reinterpret_cast<const AVContentLightMetadata *>(sd.data);
            out->have_light_level = true;
        }
    }
}

bool refine_from_frame(const AVFrame *frame, HdrSource *inout) {
    const HdrSource before = *inout;
    if (const AVFrameSideData *sd =
            av_frame_get_side_data(frame, AV_FRAME_DATA_MASTERING_DISPLAY_METADATA)) {
        inout->mastering = *reinterpret_cast<const AVMasteringDisplayMetadata *>(sd->data);
        inout->have_mastering = true;
    }
    if (const AVFrameSideData *sd =
            av_frame_get_side_data(frame, AV_FRAME_DATA_CONTENT_LIGHT_LEVEL)) {
        inout->light = *reinterpret_cast<const AVContentLightMetadata *>(sd->data);
        inout->have_light_level = true;
    }
    if (frame->color_trc != AVCOL_TRC_UNSPECIFIED) inout->trc = frame->color_trc;
    if (frame->color_primaries != AVCOL_PRI_UNSPECIFIED)
        inout->primaries = frame->color_primaries;
    if (frame->colorspace != AVCOL_SPC_UNSPECIFIED) inout->space = frame->colorspace;
    return before != *inout;
}

bool frame_has_hdr10plus(const AVFrame *frame) {
    return av_frame_get_side_data(frame, AV_FRAME_DATA_DYNAMIC_HDR_PLUS) != nullptr;
}

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
        out->hdmi_metadata_type1.max_cll =
            static_cast<uint16_t>(src.light.MaxCLL > 65535 ? 65535 : src.light.MaxCLL);
        out->hdmi_metadata_type1.max_fall =
            static_cast<uint16_t>(src.light.MaxFALL > 65535 ? 65535 : src.light.MaxFALL);
    } else {
        *notes += "MaxCLL/MaxFALL unavailable (left zero, which encodes unknown); ";
    }
    return true;
}

void log_hdr_source(const HdrSource &src) {
    mediabox::logf("asset hdr_side_data mastering=%d content_light_level=%d transfer=%s "
                   "primaries=%s space=%s",
                   src.have_mastering, src.have_light_level, av_color_transfer_name(src.trc),
                   av_color_primaries_name(src.primaries), av_color_space_name(src.space));
    if (src.have_mastering && src.mastering.has_primaries)
        mediabox::logf("asset mastering_display primaries(FFmpeg R,G,B order)="
                       "[(%.5f,%.5f) (%.5f,%.5f) (%.5f,%.5f)] white_point=(%.5f,%.5f)",
                       av_q2d(src.mastering.display_primaries[0][0]),
                       av_q2d(src.mastering.display_primaries[0][1]),
                       av_q2d(src.mastering.display_primaries[1][0]),
                       av_q2d(src.mastering.display_primaries[1][1]),
                       av_q2d(src.mastering.display_primaries[2][0]),
                       av_q2d(src.mastering.display_primaries[2][1]),
                       av_q2d(src.mastering.white_point[0]), av_q2d(src.mastering.white_point[1]));
    else
        mediabox::logf("asset mastering_display primaries ABSENT");
    if (src.have_mastering && src.mastering.has_luminance)
        mediabox::logf("asset mastering_display luminance min=%.6f cd/m2 max=%.3f cd/m2",
                       av_q2d(src.mastering.min_luminance), av_q2d(src.mastering.max_luminance));
    else
        mediabox::logf("asset mastering_display luminance ABSENT");
    if (src.have_light_level)
        mediabox::logf("asset MaxCLL=%u MaxFALL=%u", src.light.MaxCLL, src.light.MaxFALL);
    else
        mediabox::logf("asset MaxCLL/MaxFALL ABSENT");
}

void log_hdr_metadata(const struct hdr_output_metadata &m) {
    const struct hdr_metadata_infoframe &f = m.hdmi_metadata_type1;
    mediabox::logf("hdr_metadata blob_size=%zu metadata_type=%u eotf=%u infoframe_metadata_type=%u",
                   sizeof(m), m.metadata_type, f.eotf, f.metadata_type);
    mediabox::logf("hdr_metadata display_primaries(ST2086 order G,B,R)="
                   "[(%u,%u) (%u,%u) (%u,%u)] white_point=(%u,%u)",
                   f.display_primaries[0].x, f.display_primaries[0].y, f.display_primaries[1].x,
                   f.display_primaries[1].y, f.display_primaries[2].x, f.display_primaries[2].y,
                   f.white_point.x, f.white_point.y);
    mediabox::logf("hdr_metadata max_display_mastering_luminance=%u (cd/m2) "
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
    mediabox::logf("hdr_metadata raw=%s", hex.c_str());
}

}  // namespace mediabox::media
