#include "decode/framebuffer.h"

#include "common/log.h"

#include <cerrno>
#include <cinttypes>
#include <cstring>
#include <sys/stat.h>

#include <xf86drm.h>
#include <xf86drmMode.h>
#include <drm_fourcc.h>
#include <drm_mode.h>

namespace mediabox::decode {

void FramebufferCache::clear() {
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

bool FramebufferCache::get(const AVDRMFrameDescriptor *desc, const AVFrame *frame, uint32_t *fb_id,
                           std::string *error) {
    if (desc->nb_layers != 1) {
        *error = "unexpected DRM PRIME descriptor with " + std::to_string(desc->nb_layers) +
                 " layers";
        return false;
    }
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
        mediabox::fourcc_string(layer.format, fmt);
        *error = std::string("drmModeAddFB2WithModifiers format=") + fmt + " modifier=0x" +
                 std::to_string(modifiers[0]) + ": " + strerror(errno);
        return false;
    }
    if (!logged_format_) {
        char fmt[5];
        mediabox::fourcc_string(layer.format, fmt);
        mediabox::logf("framebuffer format=%s modifier=0x%" PRIx64
                       " planes=%d pitch0=%u offset1=%u size=%dx%d",
                       fmt, modifiers[0], layer.nb_planes, pitches[0], offsets[1], frame->width,
                       frame->height);
        logged_format_ = true;
    }
    *fb_id = imported.fb_id;
    cache_.emplace(key, imported);
    return true;
}

void FramebufferCache::retain_in_flight(const AVFrame *frame) {
    AVFrame *held = av_frame_clone(frame);
    if (!held) return;
    in_flight_.push_back(held);
    while (in_flight_.size() > 3) {
        AVFrame *oldest = in_flight_.front();
        in_flight_.pop_front();
        av_frame_free(&oldest);
    }
}

}  // namespace mediabox::decode
