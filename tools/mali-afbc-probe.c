// mali-afbc-probe - what this Mali stack can allocate, render to and have
// scanned out, and at which step AFBC stops. Never becomes DRM master; every
// answer is an ioctl or a library call's own.
//
//   mali-afbc-probe <render node> <display node>
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
#include <errno.h>
#include <fcntl.h>
#include <gbm.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>
#include <drm/drm.h>
#include <drm/drm_fourcc.h>
#include <drm/drm_mode.h>

#define W 2560
#define H 1440
#define AR24 DRM_FORMAT_ARGB8888

static EGLDisplay dpy;
static EGLConfig config;
static EGLContext ctx;
static int display_fd;

static void words(const char *label, const char *s) {
    printf("%s:\n", label);
    for (const char *p = s ? s : ""; *p;) {
        size_t n = strcspn(p, " ");
        if (n) printf("  %.*s\n", (int)n, p);
        p += n;
        while (*p == ' ') p++;
    }
}

// The display controller's half: is this buffer a framebuffer to it, and does
// the interface's plane take the modifier (asked of IN_FORMATS elsewhere).
static void scanout(int fd, uint32_t pitch, uint32_t offset, uint64_t mod, int use_mod) {
    struct drm_prime_handle ph = {.fd = fd};
    if (ioctl(display_fd, DRM_IOCTL_PRIME_FD_TO_HANDLE, &ph)) { printf(" prime=%s", strerror(errno)); return; }
    struct drm_mode_fb_cmd2 f = {.width = W, .height = H, .pixel_format = AR24, .flags = use_mod ? DRM_MODE_FB_MODIFIERS : 0};
    f.handles[0] = ph.handle; f.pitches[0] = pitch; f.offsets[0] = offset;
    if (use_mod) f.modifier[0] = mod;
    if (ioctl(display_fd, DRM_IOCTL_MODE_ADDFB2, &f)) printf(" addfb2=%s", strerror(errno));
    else { printf(" addfb2=OK"); ioctl(display_fd, DRM_IOCTL_MODE_RMFB, &f.fb_id); }
    struct drm_gem_close gc = {.handle = ph.handle};
    ioctl(display_fd, DRM_IOCTL_GEM_CLOSE, &gc);
}

// As an FBO colour attachment, drawn and read back.
static void rendered(int fd, uint32_t pitch, uint32_t offset, uint64_t mod, int use_mod) {
    PFNEGLCREATEIMAGEKHRPROC mk = (void *)eglGetProcAddress("eglCreateImageKHR");
    eglGetError();
    PFNEGLDESTROYIMAGEKHRPROC rm = (void *)eglGetProcAddress("eglDestroyImageKHR");
    PFNGLEGLIMAGETARGETTEXTURE2DOESPROC tgt = (void *)eglGetProcAddress("glEGLImageTargetTexture2DOES");
    EGLint a[32]; int n = 0;
    a[n++] = EGL_WIDTH; a[n++] = W; a[n++] = EGL_HEIGHT; a[n++] = H;
    a[n++] = EGL_LINUX_DRM_FOURCC_EXT; a[n++] = AR24;
    a[n++] = EGL_DMA_BUF_PLANE0_FD_EXT; a[n++] = fd;
    a[n++] = EGL_DMA_BUF_PLANE0_OFFSET_EXT; a[n++] = offset;
    a[n++] = EGL_DMA_BUF_PLANE0_PITCH_EXT; a[n++] = pitch;
    if (use_mod) {
        a[n++] = EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT; a[n++] = (EGLint)mod;
        a[n++] = EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT; a[n++] = (EGLint)(mod >> 32);
    }
    a[n] = EGL_NONE;
    EGLImageKHR im = mk ? mk(dpy, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL, a) : NULL;
    if (!im) { printf(" eglimage=0x%x", eglGetError()); return; }
    GLuint t, f; unsigned char px[4] = {0};
    glGenTextures(1, &t); glBindTexture(GL_TEXTURE_2D, t); tgt(GL_TEXTURE_2D, im);
    GLenum te = glGetError();
    glGenFramebuffers(1, &f); glBindFramebuffer(GL_FRAMEBUFFER, f);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, t, 0);
    GLenum st = glCheckFramebufferStatus(GL_FRAMEBUFFER);
    if (te || st != GL_FRAMEBUFFER_COMPLETE) printf(" gl=0x%x/0x%x", te, st);
    else {
        glViewport(0, 0, W, H); glClearColor(1, .5f, .25f, 1); glClear(GL_COLOR_BUFFER_BIT); glFinish();
        glReadPixels(8, 8, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, px);
        printf(" render=%s", px[0] == 255 && px[1] > 120 && px[1] < 136 ? "OK" : "WRONG");
    }
    glBindFramebuffer(GL_FRAMEBUFFER, 0); glDeleteFramebuffers(1, &f); glDeleteTextures(1, &t); rm(dpy, im);
}

static void bo_probe(const char *what, struct gbm_bo *bo, int e) {
    printf("  bo  %-34s ", what);
    if (!bo) { printf("errno=%d %s\n", e, strerror(e)); return; }
    uint64_t m = gbm_bo_get_modifier(bo);
    int fd = gbm_bo_get_fd(bo);
    printf("OK mod=%#llx planes=%d stride=%u", (unsigned long long)m, gbm_bo_get_plane_count(bo), gbm_bo_get_stride(bo));
    if (fd >= 0) {
        printf(" size=%lld", (long long)lseek(fd, 0, SEEK_END));
        scanout(fd, gbm_bo_get_stride(bo), 0, m, m != DRM_FORMAT_MOD_INVALID);
        rendered(fd, gbm_bo_get_stride(bo), 0, m, m != DRM_FORMAT_MOD_INVALID);
        close(fd);
    }
    printf("\n");
    gbm_bo_destroy(bo);
}

static void surf_probe(const char *what, struct gbm_surface *s, int e) {
    printf("  srf %-34s ", what);
    if (!s) { printf("errno=%d %s\n", e, strerror(e)); return; }
    EGLSurface es = eglCreateWindowSurface(dpy, config, (EGLNativeWindowType)s, NULL);
    if (es == EGL_NO_SURFACE) { printf("egl=0x%x\n", eglGetError()); gbm_surface_destroy(s); return; }
    eglMakeCurrent(dpy, es, es, ctx);
    for (int i = 0; i < 4; i++) {
        EGLint age = -1; eglQuerySurface(dpy, es, EGL_BUFFER_AGE_EXT, &age);
        glViewport(0, 0, W, H); glClearColor(0, 1, 0, 1); glClear(GL_COLOR_BUFFER_BIT);
        int ok = eglSwapBuffers(dpy, es); int free_before = gbm_surface_has_free_buffers(s);
        struct gbm_bo *bo = gbm_surface_lock_front_buffer(s);
        if (!bo) { printf("lock:%s ", strerror(errno)); break; }
        int fd = gbm_bo_get_fd(bo);
        if (i == 0) {
            printf("swap=%d mod=%#llx stride=%u size=%lld", ok, (unsigned long long)gbm_bo_get_modifier(bo), gbm_bo_get_stride(bo), (long long)lseek(fd, 0, SEEK_END));
            scanout(fd, gbm_bo_get_stride(bo), 0, gbm_bo_get_modifier(bo), gbm_bo_get_modifier(bo) != DRM_FORMAT_MOD_INVALID);
        }
        printf(" [age=%d free=%d]", age, free_before);
        close(fd);
        // Held one swap, as the panel would.
        static struct gbm_bo *held; if (i && held) gbm_surface_release_buffer(s, held); held = bo;
    }
    printf("\n");
    eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, ctx);
    eglDestroySurface(dpy, es); gbm_surface_destroy(s);
}

int main(int argc, char **argv) {
    if (argc < 3) return 2;
    int rfd = open(argv[1], O_RDWR | O_CLOEXEC);
    display_fd = open(argv[2], O_RDWR | O_CLOEXEC);
    // Opened while the interface holds master this fd never is one; dropped
    // in case nobody did.
    ioctl(display_fd, DRM_IOCTL_DROP_MASTER, 0);
    struct gbm_device *g = gbm_create_device(rfd);
    printf("render=%s gbm=%s\n", argv[1], gbm_device_get_backend_name(g));
    PFNEGLGETPLATFORMDISPLAYEXTPROC gp = (void *)eglGetProcAddress("eglGetPlatformDisplayEXT");
    dpy = gp ? gp(EGL_PLATFORM_GBM_KHR, g, NULL) : eglGetPlatformDisplay(EGL_PLATFORM_GBM_KHR, g, NULL);
    EGLint ma, mi; if (!eglInitialize(dpy, &ma, &mi)) { printf("eglInitialize 0x%x\n", eglGetError()); return 1; }
    eglBindAPI(EGL_OPENGL_ES_API);
    EGLConfig all[128]; EGLint n = 0;
    EGLint ca[] = {EGL_SURFACE_TYPE, EGL_WINDOW_BIT, EGL_ALPHA_SIZE, 8, EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT, EGL_NONE};
    eglChooseConfig(dpy, ca, all, 128, &n);
    for (int i = 0; i < n; i++) {
        EGLint v, s, st, d; eglGetConfigAttrib(dpy, all[i], EGL_NATIVE_VISUAL_ID, &v); eglGetConfigAttrib(dpy, all[i], EGL_SAMPLES, &s);
        eglGetConfigAttrib(dpy, all[i], EGL_STENCIL_SIZE, &st); eglGetConfigAttrib(dpy, all[i], EGL_DEPTH_SIZE, &d);
        printf("config %.4s samples=%d stencil=%d depth=%d\n", (char *)&v, s, st, d);
        if (v == AR24 && s == 0 && !config) config = all[i];
    }
    EGLint xa[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    ctx = eglCreateContext(dpy, config, EGL_NO_CONTEXT, xa);
    if (!eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, ctx)) { printf("makecurrent 0x%x\n", eglGetError()); return 1; }
    printf("EGL %s | %s\nGL %s | %s | %s\n", eglQueryString(dpy, EGL_VENDOR), eglQueryString(dpy, EGL_VERSION), glGetString(GL_RENDERER), glGetString(GL_VERSION), glGetString(GL_SHADING_LANGUAGE_VERSION));
    words("EGL_EXTENSIONS", eglQueryString(dpy, EGL_EXTENSIONS));
    words("GL_EXTENSIONS", (const char *)glGetString(GL_EXTENSIONS));

    PFNEGLQUERYDMABUFMODIFIERSEXTPROC qm = (void *)eglGetProcAddress("eglQueryDmaBufModifiersEXT");
    EGLuint64KHR mods[256]; EGLBoolean ext[256]; EGLint nm = 0;
    if (qm && qm(dpy, AR24, 256, mods, ext, &nm)) {
        printf("egl modifiers AR24: %d\n", nm);
        for (int i = 0; i < nm; i++) printf("  %#018llx external_only=%d gbm_planes=%d\n", (unsigned long long)mods[i], ext[i], gbm_device_get_format_modifier_plane_count(g, AR24, mods[i]));
    } else printf("eglQueryDmaBufModifiersEXT: none (0x%x)\n", eglGetError());

    uint32_t S = GBM_BO_USE_SCANOUT, R = GBM_BO_USE_RENDERING, L = GBM_BO_USE_LINEAR;
    struct { const char *n; uint32_t f; } fl[] = {{"RENDERING", R}, {"SCANOUT", S}, {"SCANOUT|RENDERING", S | R}, {"SCANOUT|RENDERING|LINEAR", S | R | L}};
    for (int i = 0; i < 4; i++) { errno = 0; struct gbm_bo *b = gbm_bo_create(g, W, H, AR24, fl[i].f); bo_probe(fl[i].n, b, errno); }
    for (int i = 0; i < 4; i++) { errno = 0; struct gbm_surface *s = gbm_surface_create(g, W, H, AR24, fl[i].f); surf_probe(fl[i].n, s, errno); }
    // Every modifier EGL offers and the ones Cluster0's IN_FORMATS names, alone.
    uint64_t want[300]; int nw = 0;
    want[nw++] = DRM_FORMAT_MOD_LINEAR;
    for (int i = 0; i < nm; i++) want[nw++] = mods[i];
    for (int i = 3; i < argc && nw < 300; i++) want[nw++] = strtoull(argv[i], NULL, 0);
    for (int i = 0; i < nw; i++) {
        char l[64]; snprintf(l, sizeof l, "mod %#llx", (unsigned long long)want[i]);
        errno = 0; struct gbm_bo *b = gbm_bo_create_with_modifiers(g, W, H, AR24, &want[i], 1); bo_probe(l, b, errno);
        char l2[80]; snprintf(l2, sizeof l2, "%s +RENDERING", l);
        errno = 0; b = gbm_bo_create_with_modifiers2(g, W, H, AR24, &want[i], 1, R); bo_probe(l2, b, errno);
        snprintf(l2, sizeof l2, "%s +SCANOUT|R", l);
        errno = 0; b = gbm_bo_create_with_modifiers2(g, W, H, AR24, &want[i], 1, R | S); bo_probe(l2, b, errno);
        errno = 0; struct gbm_surface *s = gbm_surface_create_with_modifiers(g, W, H, AR24, &want[i], 1); surf_probe(l, s, errno);
    }
    // Around GBM: a buffer the display controller allocated, large enough for
    // an AFBC header and body, offered to EGL under each AFBC modifier.
    struct drm_mode_create_dumb cd = {.width = W, .height = H + 64, .bpp = 32};
    struct drm_prime_handle ph = {0};
    if (!ioctl(display_fd, DRM_IOCTL_MODE_CREATE_DUMB, &cd) &&
        (ph.handle = cd.handle, ph.flags = DRM_CLOEXEC | DRM_RDWR, !ioctl(display_fd, DRM_IOCTL_PRIME_HANDLE_TO_FD, &ph))) {
        for (int i = 0; i < nw; i++) {
            printf("  dumb mod %#-20llx", (unsigned long long)want[i]);
            scanout(ph.fd, cd.pitch, 0, want[i], 1);
            rendered(ph.fd, cd.pitch, 0, want[i], 1);
            printf("\n");
        }
        close(ph.fd);
    }
    return 0;
}
