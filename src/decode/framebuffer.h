// Turns RKMPP's DRM PRIME output into scanout-ready DRM framebuffers.
//
// Two subtleties, both learned the hard way in Gate MP1a and preserved here:
//
//   * MPP recycles a bounded buffer group, so dma-buf *fd numbers* are reused.
//     The cache is therefore keyed on the dma-buf inode, which identifies the
//     underlying buffer for as long as it exists.
//   * The GEM handle keeps a buffer alive, but MPP may hand it straight back
//     to the decoder once the last AVFrame reference drops. A few recent
//     frames are retained so the buffer currently on screen is out of that
//     rotation, without pinning the whole pool and starving the decoder.
#pragma once

#include <cstdint>
#include <deque>
#include <map>
#include <string>

extern "C" {
#include <libavutil/frame.h>
#include <libavutil/hwcontext_drm.h>
}

namespace mediabox::decode {

class FramebufferCache {
  public:
    explicit FramebufferCache(int drm_fd) : drm_fd_(drm_fd) {}
    FramebufferCache(const FramebufferCache &) = delete;
    FramebufferCache &operator=(const FramebufferCache &) = delete;
    ~FramebufferCache() { clear(); }

    // Must run while the DRM fd is still open; the display's restore path
    // calls it before closing.
    void clear();

    bool get(const AVDRMFrameDescriptor *desc, const AVFrame *frame, uint32_t *fb_id,
             std::string *error);

    void retain_in_flight(const AVFrame *frame);

    size_t size() const { return cache_.size(); }

  private:
    struct ImportedFrame {
        uint32_t fb_id = 0;
        uint32_t handles[4] = {0, 0, 0, 0};
    };

    int drm_fd_;
    std::map<uint64_t, ImportedFrame> cache_;
    std::deque<AVFrame *> in_flight_;
    bool logged_format_ = false;
};

}  // namespace mediabox::decode
