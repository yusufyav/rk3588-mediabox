// gbm-buffer-recycle-probe - checks the two GBM contracts Kodi's page-flip
// loop silently depends on, on whatever GL user space the loader resolves.
//
// Kodi's CGBMUtils::CGBMDevice::CGBMSurface::LockFrontBuffer() locks a new
// front buffer every frame and releases the oldest one only when
// gbm_surface_has_free_buffers() reports the pool exhausted. It caches the DRM
// framebuffer for each buffer on the gbm_bo itself, via gbm_bo_set_user_data.
// Neither is checked anywhere, and neither fails loudly:
//
//   * a has_free_buffers() that never returns 0 makes the release unreachable,
//     so one full GUI-sized scanout buffer leaks per frame. On RK3588 that
//     exhausts the 4 GiB rockchip display IOVA arena in about eight seconds,
//     and the first thing anyone sees is eglSwapBuffers returning
//     EGL_BAD_ALLOC, which Kodi turns into an unhandled std::runtime_error.
//     ARM's libmali is such an implementation.
//
//   * a get_user_data() that never returns what was set turns the framebuffer
//     cache into a per-frame drmModeAddFB2, which leaks in the same arena for
//     a completely different reason.
//
// Both are cheap to test and impossible to infer from a renderer string, so
// they are tested rather than assumed. Run it against a GL stack before
// trusting Kodi's GUI to it.
//
//   gbm-buffer-recycle-probe [frames] [/dev/dri/card0]

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <gbm.h>

#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

// Kodi's own GUI buffer size on this appliance. Using the real size matters:
// the failure is an address-space exhaustion, so the per-frame cost is what
// decides whether a leak is visible in seconds or in hours.
#define GUI_W 1280
#define GUI_H 720
#define MAXQ 4096

static void print_arena(const char *tag) {
    FILE *f = fopen("/sys/kernel/debug/dri/0/mm_dump", "r");
    if (!f) return;
    char line[256], last[256] = "";
    while (fgets(line, sizeof line, f)) snprintf(last, sizeof last, "%s", line);
    fclose(f);
    printf("  %-14s %s", tag, last);
}

static int destroyed = 0;
static void user_data_destroy(struct gbm_bo *bo, void *d) { (void)bo; (void)d; destroyed++; }

int main(int argc, char **argv) {
    int frames = argc > 1 ? atoi(argv[1]) : 120;
    const char *node = argc > 2 ? argv[2] : "/dev/dri/card0";

    int fd = open(node, O_RDWR | O_CLOEXEC);
    if (fd < 0) { printf("cannot open %s\n", node); return 1; }
    struct gbm_device *gbm = gbm_create_device(fd);
    if (!gbm) { printf("gbm_create_device failed\n"); return 1; }

    PFNEGLGETPLATFORMDISPLAYEXTPROC gpd =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    EGLDisplay dpy = gpd ? gpd(EGL_PLATFORM_GBM_KHR, gbm, NULL)
                         : eglGetDisplay((EGLNativeDisplayType)gbm);
    if (dpy == EGL_NO_DISPLAY) { printf("no EGL display\n"); return 1; }
    if (!eglInitialize(dpy, NULL, NULL)) { printf("eglInitialize failed\n"); return 1; }
    eglBindAPI(EGL_OPENGL_ES_API);

    const EGLint ca[] = {EGL_SURFACE_TYPE, EGL_WINDOW_BIT, EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8,
                         EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 0,
                         EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT, EGL_NONE};
    EGLConfig cfg; EGLint nc = 0;
    if (!eglChooseConfig(dpy, ca, &cfg, 1, &nc) || nc < 1) { printf("no config\n"); return 1; }
    EGLint vis = 0;
    eglGetConfigAttrib(dpy, cfg, EGL_NATIVE_VISUAL_ID, &vis);
    const EGLint xa[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    EGLContext ctx = eglCreateContext(dpy, cfg, EGL_NO_CONTEXT, xa);
    struct gbm_surface *surf = gbm_surface_create(gbm, GUI_W, GUI_H, (uint32_t)vis,
                                   GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING);
    if (!surf) { printf("gbm_surface_create failed\n"); return 1; }
    EGLSurface es = eglCreateWindowSurface(dpy, cfg, (EGLNativeWindowType)surf, NULL);
    if (es == EGL_NO_SURFACE || !eglMakeCurrent(dpy, es, es, ctx)) {
        printf("cannot make a window surface current\n");
        return 1;
    }

    const char *renderer = (const char *)glGetString(GL_RENDERER);
    printf("renderer        : %s\n", renderer ? renderer : "(null)");
    printf("surface         : %dx%d, %d frames\n", GUI_W, GUI_H, frames);
    print_arena("arena before");

    int marker = 0xA5A5;
    int ud_hits = 0, ud_misses = 0;
    struct gbm_bo *q[MAXQ];
    int head = 0, tail = 0, depth_max = 0, first_full = -1, overflow = 0;

    for (int i = 0; i < frames; i++) {
        glClearColor((i % 60) / 60.0f, 0.2f, 0.3f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        if (!eglSwapBuffers(dpy, es)) {
            printf("  swap %d failed 0x%x\n", i, eglGetError());
            break;
        }
        struct gbm_bo *bo = gbm_surface_lock_front_buffer(surf);
        if (!bo) { printf("  lock %d returned NULL\n", i); break; }

        if (gbm_bo_get_user_data(bo) == &marker)
            ud_hits++;
        else {
            ud_misses++;
            gbm_bo_set_user_data(bo, &marker, user_data_destroy);
        }

        if (tail - head >= MAXQ) { overflow = 1; break; }
        q[tail++ % MAXQ] = bo;

        // Kodi's release condition, reproduced exactly.
        if (!gbm_surface_has_free_buffers(surf)) {
            if (first_full < 0) first_full = i + 1;
            gbm_surface_release_buffer(surf, q[head++ % MAXQ]);
        }
        if (tail - head > depth_max) depth_max = tail - head;
    }
    print_arena("arena after");

    printf("has_free_buffers first reported full at frame : %s\n",
           first_full < 0 ? "NEVER" : "");
    if (first_full > 0) printf("                                                %d\n", first_full);
    printf("max locked-buffer queue depth                 : %d%s\n", depth_max,
           overflow ? " (queue overflowed)" : "");
    printf("gbm_bo user data hits / misses                : %d / %d\n", ud_hits, ud_misses);

    while (head < tail) gbm_surface_release_buffer(surf, q[head++ % MAXQ]);
    eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    eglDestroySurface(dpy, es);
    gbm_surface_destroy(surf);
    eglDestroyContext(dpy, ctx);
    eglTerminate(dpy);
    gbm_device_destroy(gbm);
    close(fd);

    const int recycles = first_full > 0 && depth_max < frames;

    printf("RECYCLE CONTRACT : %s\n",
           recycles ? "HONOURED - Kodi's release path is reachable"
                    : "BROKEN - has_free_buffers() never reports full; Kodi would "
                      "leak one buffer per frame");

    // The user-data question is only meaningful once a buffer has been reused.
    // If nothing is ever recycled then every lock hands back a fresh bo and the
    // cache misses by definition -- reporting that as a second, independent
    // fault would double-count one bug.
    if (!recycles)
        printf("USERDATA CONTRACT: NOT TESTABLE - no buffer was reused, so a miss "
               "is expected (%d hits / %d misses)\n", ud_hits, ud_misses);
    else
        printf("USERDATA CONTRACT: %s (%d hits / %d misses)\n",
               ud_hits > 0 ? "HONOURED - the per-bo framebuffer cache works"
                           : "BROKEN - Kodi would re-add a DRM framebuffer every frame",
               ud_hits, ud_misses);

    return (recycles && ud_hits > 0) ? 0 : 2;
}
