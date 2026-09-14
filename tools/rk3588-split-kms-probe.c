/*
 * rk3588-split-kms-probe - prove the vendor Mali render node can feed the
 * Rockchip display node without a compositor.
 *
 * Render:  /dev/dri/renderD128 -> vendor GBM/EGL -> /dev/mali0
 * Display: /dev/dri/card0      -> PRIME import -> KMS
 *
 * The two locked GBM front buffers stay alive until the page-flip event.  All
 * framebuffer ids, imported GEM handles and dma-buf fds are then released in
 * the reverse order in which they were created.
 */

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <drm_fourcc.h>
#include <errno.h>
#include <fcntl.h>
#include <gbm.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>
#include <xf86drm.h>
#include <xf86drmMode.h>

struct output {
    uint32_t connector_id;
    uint32_t crtc_id;
    drmModeModeInfo mode;
};

struct frame {
    struct gbm_bo *bo;
    int dma_fd;
    uint32_t gem_handle;
    uint32_t fb_id;
    uint32_t stride;
    uint32_t offset;
    uint64_t modifier;
};

static const char *egl_error(EGLint e) {
    switch (e) {
    case EGL_SUCCESS: return "EGL_SUCCESS";
    case EGL_NOT_INITIALIZED: return "EGL_NOT_INITIALIZED";
    case EGL_BAD_ACCESS: return "EGL_BAD_ACCESS";
    case EGL_BAD_ALLOC: return "EGL_BAD_ALLOC";
    case EGL_BAD_ATTRIBUTE: return "EGL_BAD_ATTRIBUTE";
    case EGL_BAD_CONFIG: return "EGL_BAD_CONFIG";
    case EGL_BAD_CONTEXT: return "EGL_BAD_CONTEXT";
    case EGL_BAD_DISPLAY: return "EGL_BAD_DISPLAY";
    case EGL_BAD_MATCH: return "EGL_BAD_MATCH";
    case EGL_BAD_NATIVE_WINDOW: return "EGL_BAD_NATIVE_WINDOW";
    case EGL_BAD_PARAMETER: return "EGL_BAD_PARAMETER";
    case EGL_BAD_SURFACE: return "EGL_BAD_SURFACE";
    default: return "unknown EGL error";
    }
}

static int fail_errno(const char *what) {
    fprintf(stderr, "SPLIT-KMS RESULT: FAIL: %s: errno=%d (%s)\n",
            what, errno, strerror(errno));
    return -1;
}

static int find_output(int fd, struct output *out) {
    drmModeRes *res = drmModeGetResources(fd);
    if (!res)
        return fail_errno("drmModeGetResources(card0)");

    int rc = -1;
    for (int i = 0; i < res->count_connectors && rc; i++) {
        drmModeConnector *conn = drmModeGetConnector(fd, res->connectors[i]);
        if (!conn)
            continue;
        if (conn->connection != DRM_MODE_CONNECTED || conn->count_modes == 0 ||
            conn->connector_type != DRM_MODE_CONNECTOR_HDMIA) {
            drmModeFreeConnector(conn);
            continue;
        }

        int mode_index = 0;
        for (int m = 0; m < conn->count_modes; m++) {
            if (conn->modes[m].type & DRM_MODE_TYPE_PREFERRED) {
                mode_index = m;
                break;
            }
        }

        drmModeEncoder *enc = NULL;
        if (conn->encoder_id)
            enc = drmModeGetEncoder(fd, conn->encoder_id);
        if (!enc) {
            for (int e = 0; e < conn->count_encoders && !enc; e++)
                enc = drmModeGetEncoder(fd, conn->encoders[e]);
        }
        if (!enc) {
            drmModeFreeConnector(conn);
            continue;
        }

        uint32_t crtc_id = enc->crtc_id;
        if (!crtc_id) {
            for (int c = 0; c < res->count_crtcs; c++) {
                if (enc->possible_crtcs & (1u << c)) {
                    crtc_id = res->crtcs[c];
                    break;
                }
            }
        }
        if (crtc_id) {
            out->connector_id = conn->connector_id;
            out->crtc_id = crtc_id;
            out->mode = conn->modes[mode_index];
            rc = 0;
        }
        drmModeFreeEncoder(enc);
        drmModeFreeConnector(conn);
    }
    drmModeFreeResources(res);
    if (rc)
        fprintf(stderr, "SPLIT-KMS RESULT: FAIL: no connected HDMI-A output with CRTC\n");
    return rc;
}

static EGLDisplay get_gbm_display(struct gbm_device *gbm) {
    PFNEGLGETPLATFORMDISPLAYPROC core =
        (PFNEGLGETPLATFORMDISPLAYPROC)eglGetProcAddress("eglGetPlatformDisplay");
    if (core) {
        EGLDisplay dpy = core(EGL_PLATFORM_GBM_KHR, gbm, NULL);
        if (dpy != EGL_NO_DISPLAY)
            return dpy;
    }
    PFNEGLGETPLATFORMDISPLAYEXTPROC ext =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    if (ext)
        return ext(EGL_PLATFORM_GBM_KHR, gbm, NULL);
    return EGL_NO_DISPLAY;
}

static EGLConfig choose_xrgb_config(EGLDisplay dpy) {
    const EGLint attrs[] = {
        EGL_SURFACE_TYPE, EGL_WINDOW_BIT,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
        EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8,
        EGL_NONE,
    };
    EGLint count = 0;
    if (!eglChooseConfig(dpy, attrs, NULL, 0, &count) || count < 1)
        return NULL;
    EGLConfig *configs = calloc((size_t)count, sizeof(*configs));
    if (!configs)
        return NULL;
    if (!eglChooseConfig(dpy, attrs, configs, count, &count)) {
        free(configs);
        return NULL;
    }
    EGLConfig chosen = NULL;
    for (EGLint i = 0; i < count; i++) {
        EGLint visual = 0;
        if (eglGetConfigAttrib(dpy, configs[i], EGL_NATIVE_VISUAL_ID, &visual) &&
            (uint32_t)visual == GBM_FORMAT_XRGB8888) {
            chosen = configs[i];
            break;
        }
    }
    free(configs);
    return chosen;
}

static int import_frame(int kms_fd, struct frame *frame) {
    frame->stride = gbm_bo_get_stride_for_plane(frame->bo, 0);
    frame->offset = gbm_bo_get_offset(frame->bo, 0);
    frame->modifier = gbm_bo_get_modifier(frame->bo);
    frame->dma_fd = gbm_bo_get_fd_for_plane(frame->bo, 0);
    if (frame->dma_fd < 0)
        return fail_errno("gbm_bo_get_fd_for_plane");
    if (drmPrimeFDToHandle(kms_fd, frame->dma_fd, &frame->gem_handle))
        return fail_errno("DRM_IOCTL_PRIME_FD_TO_HANDLE(card0)");

    uint32_t handles[4] = { frame->gem_handle, 0, 0, 0 };
    uint32_t strides[4] = { frame->stride, 0, 0, 0 };
    uint32_t offsets[4] = { frame->offset, 0, 0, 0 };
    uint64_t modifiers[4] = { frame->modifier, 0, 0, 0 };
    uint32_t width = gbm_bo_get_width(frame->bo);
    uint32_t height = gbm_bo_get_height(frame->bo);

    int rc;
    if (frame->modifier != DRM_FORMAT_MOD_INVALID) {
        rc = drmModeAddFB2WithModifiers(kms_fd, width, height,
                                        DRM_FORMAT_XRGB8888, handles, strides,
                                        offsets, modifiers, &frame->fb_id,
                                        DRM_MODE_FB_MODIFIERS);
    } else {
        rc = drmModeAddFB2(kms_fd, width, height, DRM_FORMAT_XRGB8888,
                           handles, strides, offsets, &frame->fb_id, 0);
    }
    if (rc)
        return fail_errno("DRM_IOCTL_MODE_ADDFB2(card0 imported dma-buf)");

    printf("dma-buf             : fd=%d stride=%u offset=%u modifier=0x%016llx\n",
           frame->dma_fd, frame->stride, frame->offset,
           (unsigned long long)frame->modifier);
    printf("card0 import        : GEM handle=%u FB_ID=%u\n",
           frame->gem_handle, frame->fb_id);
    return 0;
}

static void release_frame(int kms_fd, struct gbm_surface *surface,
                          struct frame *frame) {
    if (frame->fb_id)
        drmModeRmFB(kms_fd, frame->fb_id);
    if (frame->gem_handle) {
        struct drm_gem_close close_args = { .handle = frame->gem_handle };
        ioctl(kms_fd, DRM_IOCTL_GEM_CLOSE, &close_args);
    }
    if (frame->dma_fd >= 0)
        close(frame->dma_fd);
    if (frame->bo)
        gbm_surface_release_buffer(surface, frame->bo);
    memset(frame, 0, sizeof(*frame));
    frame->dma_fd = -1;
}

static volatile bool flip_done;

static void page_flip_handler(int fd, unsigned int sequence, unsigned int sec,
                              unsigned int usec, void *data) {
    (void)fd; (void)data;
    flip_done = true;
    printf("page-flip complete  : sequence=%u time=%u.%06u\n", sequence, sec, usec);
}

static int wait_for_flip(int fd) {
    drmEventContext event = {
        .version = DRM_EVENT_CONTEXT_VERSION,
        .page_flip_handler = page_flip_handler,
    };
    struct pollfd pfd = { .fd = fd, .events = POLLIN };
    while (!flip_done) {
        int rc = poll(&pfd, 1, 3000);
        if (rc < 0 && errno == EINTR)
            continue;
        if (rc <= 0)
            return fail_errno(rc == 0 ? "page-flip event timeout" : "poll(card0)");
        if (drmHandleEvent(fd, &event))
            return fail_errno("drmHandleEvent(card0)");
    }
    return 0;
}

int main(int argc, char **argv) {
    const char *render_node = argc > 1 ? argv[1] : "/dev/dri/renderD128";
    const char *kms_node = argc > 2 ? argv[2] : "/dev/dri/card0";
    int rc = 1, render_fd = -1, kms_fd = -1;
    struct gbm_device *gbm = NULL;
    struct gbm_surface *surface = NULL;
    EGLDisplay dpy = EGL_NO_DISPLAY;
    EGLContext ctx = EGL_NO_CONTEXT;
    EGLSurface egl_surface = EGL_NO_SURFACE;
    struct frame frames[2] = { { .dma_fd = -1 }, { .dma_fd = -1 } };
    struct output output = {0};

    printf("== RK3588 split render/display KMS probe ==\n");
    render_fd = open(render_node, O_RDWR | O_CLOEXEC);
    if (render_fd < 0) { fail_errno("open(render node)"); goto out; }
    kms_fd = open(kms_node, O_RDWR | O_CLOEXEC);
    if (kms_fd < 0) { fail_errno("open(card0)"); goto out; }
    printf("render node         : %s\n", render_node);
    printf("display node        : %s\n", kms_node);

    if (find_output(kms_fd, &output))
        goto out;
    printf("HDMI output         : connector=%u crtc=%u mode=%s %ux%u@%u\n",
           output.connector_id, output.crtc_id, output.mode.name,
           output.mode.hdisplay, output.mode.vdisplay, output.mode.vrefresh);

    gbm = gbm_create_device(render_fd);
    if (!gbm) { fail_errno("gbm_create_device(renderD128)"); goto out; }
    printf("GBM backend         : %s\n", gbm_device_get_backend_name(gbm));

    dpy = get_gbm_display(gbm);
    if (dpy == EGL_NO_DISPLAY) {
        fprintf(stderr, "SPLIT-KMS RESULT: FAIL: eglGetPlatformDisplay(GBM): %s 0x%x\n",
                egl_error(eglGetError()), eglGetError());
        goto out;
    }
    EGLint major = 0, minor = 0, config_count = 0;
    if (!eglInitialize(dpy, &major, &minor)) {
        EGLint e = eglGetError();
        fprintf(stderr, "SPLIT-KMS RESULT: FAIL: eglInitialize: %s 0x%x\n",
                egl_error(e), e);
        goto out;
    }
    eglGetConfigs(dpy, NULL, 0, &config_count);
    printf("EGL                 : %d.%d vendor=%s version=%s configs=%d\n",
           major, minor, eglQueryString(dpy, EGL_VENDOR),
           eglQueryString(dpy, EGL_VERSION), config_count);

    if (!eglBindAPI(EGL_OPENGL_ES_API)) {
        fprintf(stderr, "SPLIT-KMS RESULT: FAIL: eglBindAPI: 0x%x\n", eglGetError());
        goto out;
    }
    EGLConfig config = choose_xrgb_config(dpy);
    if (!config) {
        fprintf(stderr, "SPLIT-KMS RESULT: FAIL: no XRGB8888 window config\n");
        goto out;
    }
    const EGLint ctx_attrs[] = { EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE };
    ctx = eglCreateContext(dpy, config, EGL_NO_CONTEXT, ctx_attrs);
    if (ctx == EGL_NO_CONTEXT) {
        fprintf(stderr, "SPLIT-KMS RESULT: FAIL: eglCreateContext: 0x%x\n", eglGetError());
        goto out;
    }

    surface = gbm_surface_create(gbm, output.mode.hdisplay, output.mode.vdisplay,
                                 GBM_FORMAT_XRGB8888,
                                 GBM_BO_USE_RENDERING | GBM_BO_USE_SCANOUT |
                                 GBM_BO_USE_LINEAR);
    if (!surface) { fail_errno("gbm_surface_create(XRGB8888)"); goto out; }
    egl_surface = eglCreateWindowSurface(dpy, config,
                                         (EGLNativeWindowType)surface, NULL);
    if (egl_surface == EGL_NO_SURFACE) {
        fprintf(stderr, "SPLIT-KMS RESULT: FAIL: eglCreateWindowSurface: 0x%x\n",
                eglGetError());
        goto out;
    }
    if (!eglMakeCurrent(dpy, egl_surface, egl_surface, ctx)) {
        fprintf(stderr, "SPLIT-KMS RESULT: FAIL: eglMakeCurrent: 0x%x\n", eglGetError());
        goto out;
    }
    printf("GLES                : vendor=%s renderer=%s version=%s\n",
           glGetString(GL_VENDOR), glGetString(GL_RENDERER), glGetString(GL_VERSION));

    const float colours[2][4] = {
        { 0.04f, 0.12f, 0.30f, 1.0f },
        { 0.00f, 0.42f, 0.24f, 1.0f },
    };
    for (int i = 0; i < 2; i++) {
        glViewport(0, 0, output.mode.hdisplay, output.mode.vdisplay);
        glClearColor(colours[i][0], colours[i][1], colours[i][2], colours[i][3]);
        glClear(GL_COLOR_BUFFER_BIT);
        glFinish();
        if (!eglSwapBuffers(dpy, egl_surface)) {
            fprintf(stderr, "SPLIT-KMS RESULT: FAIL: eglSwapBuffers[%d]: 0x%x\n",
                    i, eglGetError());
            goto out;
        }
        frames[i].bo = gbm_surface_lock_front_buffer(surface);
        if (!frames[i].bo) { fail_errno("gbm_surface_lock_front_buffer"); goto out; }
        printf("frame %d             : %ux%u format=0x%08x\n", i,
               gbm_bo_get_width(frames[i].bo), gbm_bo_get_height(frames[i].bo),
               gbm_bo_get_format(frames[i].bo));
        if (import_frame(kms_fd, &frames[i]))
            goto out;
    }

    if (drmSetMaster(kms_fd) && errno != EINVAL) {
        fail_errno("drmSetMaster(card0)");
        goto out;
    }
    if (drmModeSetCrtc(kms_fd, output.crtc_id, frames[0].fb_id, 0, 0,
                       &output.connector_id, 1, &output.mode)) {
        fail_errno("DRM_IOCTL_MODE_SETCRTC(first imported FB)");
        goto out;
    }
    printf("physical scanout    : first imported FB active\n");
    usleep(500000);

    flip_done = false;
    if (drmModePageFlip(kms_fd, output.crtc_id, frames[1].fb_id,
                        DRM_MODE_PAGE_FLIP_EVENT, NULL)) {
        fail_errno("DRM_IOCTL_MODE_PAGE_FLIP(second imported FB)");
        goto out;
    }
    if (wait_for_flip(kms_fd))
        goto out;

    printf("BO lifetime         : both render BOs held through flip completion\n");
    printf("SPLIT-KMS RESULT: PASS (Mali EGL -> dma-buf -> card0 PRIME -> HDMI)\n");
    sleep(2);
    rc = 0;

out:
    if (kms_fd >= 0 && output.crtc_id)
        drmModeSetCrtc(kms_fd, output.crtc_id, 0, 0, 0, NULL, 0, NULL);
    if (egl_surface != EGL_NO_SURFACE)
        eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    if (kms_fd >= 0 && surface) {
        release_frame(kms_fd, surface, &frames[1]);
        release_frame(kms_fd, surface, &frames[0]);
    }
    if (egl_surface != EGL_NO_SURFACE)
        eglDestroySurface(dpy, egl_surface);
    if (surface)
        gbm_surface_destroy(surface);
    if (ctx != EGL_NO_CONTEXT)
        eglDestroyContext(dpy, ctx);
    if (dpy != EGL_NO_DISPLAY)
        eglTerminate(dpy);
    if (gbm)
        gbm_device_destroy(gbm);
    if (kms_fd >= 0) {
        drmDropMaster(kms_fd);
        close(kms_fd);
    }
    if (render_fd >= 0)
        close(render_fd);
    return rc;
}
