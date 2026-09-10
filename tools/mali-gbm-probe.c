// mali-gbm-probe - proves, or disproves, that a *specific* GL user-space
// stack drives the Mali GPU on this board.
//
// tools/gbm-egl-probe.c already answers "is there a working GBM/EGL/GLES
// stack at all"; on this target it answers yes and llvmpipe. This probe asks
// the narrower question that Gate MP2 is blocked on: when the loader is
// pointed at an isolated libmali runtime instead of the system Mesa, does the
// vendor driver initialise against this kernel, and is the resulting context
// really running on the GPU?
//
// So it does three things gbm-egl-probe does not:
//
//   * dumps the full EGL and GL extension strings, because a Mali context that
//     silently falls back is distinguishable from a real one only there,
//   * dumps its own /proc/self/maps, so the answer names the shared objects
//     that were actually mapped rather than the ones that were requested,
//   * classifies the outcome, rather than reporting PASS for software.
//
// It never sets a mode and never becomes DRM master, so it is safe to run
// while something else owns the display.
//
//   mali-gbm-probe [/dev/dri/card0]

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <gbm.h>

#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static const char *or_null(const char *s) { return s ? s : "(null)"; }

// One extension per line. These strings run to several kilobytes and the
// difference between two runs is the whole point, so they have to diff
// line-by-line rather than as one unreadable paragraph.
static void print_list(const char *label, const char *list) {
    if (!list) {
        printf("%s: (null)\n", label);
        return;
    }
    printf("%s:\n", label);
    const char *p = list;
    while (*p) {
        while (*p == ' ')
            p++;
        const char *start = p;
        while (*p && *p != ' ')
            p++;
        if (p > start)
            printf("  %.*s\n", (int)(p - start), start);
    }
}

// The loader's own account of what it resolved. A private LD_LIBRARY_PATH can
// be set correctly and still lose to an RPATH, an ld.so.conf entry or a glvnd
// dispatch; only the mapped file names settle it.
static void print_maps(const char *label) {
    printf("%s:\n", label);
    FILE *f = fopen("/proc/self/maps", "r");
    if (!f) {
        printf("  (cannot open /proc/self/maps)\n");
        return;
    }
    char line[512];
    char seen[64][256];
    int n_seen = 0;
    while (fgets(line, sizeof line, f)) {
        const char *slash = strchr(line, '/');
        if (!slash)
            continue;
        char path[256];
        snprintf(path, sizeof path, "%s", slash);
        char *nl = strchr(path, '\n');
        if (nl)
            *nl = '\0';
        if (!strstr(path, ".so"))
            continue;
        int dup = 0;
        for (int i = 0; i < n_seen; i++)
            if (!strcmp(seen[i], path))
                dup = 1;
        if (dup || n_seen >= 64)
            continue;
        snprintf(seen[n_seen++], sizeof seen[0], "%s", path);
        printf("  %s\n", path);
    }
    fclose(f);
}

int main(int argc, char **argv) {
    const char *node = argc > 1 ? argv[1] : "/dev/dri/card0";

    printf("== mali-gbm-probe ==\n");

    int fd = open(node, O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        printf("drm node            : %s (open FAILED)\n", node);
        printf("MALI PROBE RESULT: FAIL (cannot open %s)\n", node);
        return 1;
    }
    printf("drm node            : %s (fd %d)\n", node, fd);

    struct gbm_device *gbm = gbm_create_device(fd);
    if (!gbm) {
        printf("MALI PROBE RESULT: FAIL (gbm_create_device)\n");
        return 1;
    }
    printf("gbm backend         : %s\n", or_null(gbm_device_get_backend_name(gbm)));

    // Kodi's CWinSystemGbm takes the GBM platform. Reaching the display any
    // other way would prove nothing about the path Kodi takes. libmali exports
    // core eglGetPlatformDisplay (EGL 1.5) but not the EXT entry point, so both
    // spellings are tried before the legacy call.
    EGLDisplay dpy = EGL_NO_DISPLAY;
    const char *how = "";
    PFNEGLGETPLATFORMDISPLAYEXTPROC get_platform_display_ext =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    if (get_platform_display_ext) {
        dpy = get_platform_display_ext(EGL_PLATFORM_GBM_KHR, gbm, NULL);
        how = "eglGetPlatformDisplayEXT";
    }
    if (dpy == EGL_NO_DISPLAY) {
        PFNEGLGETPLATFORMDISPLAYPROC get_platform_display =
            (PFNEGLGETPLATFORMDISPLAYPROC)eglGetProcAddress("eglGetPlatformDisplay");
        if (get_platform_display) {
            dpy = get_platform_display(EGL_PLATFORM_GBM_KHR, gbm, NULL);
            how = "eglGetPlatformDisplay";
        }
    }
    if (dpy == EGL_NO_DISPLAY) {
        dpy = eglGetDisplay((EGLNativeDisplayType)gbm);
        how = "eglGetDisplay";
    }
    if (dpy == EGL_NO_DISPLAY) {
        printf("MALI PROBE RESULT: FAIL (no EGL display on the GBM platform)\n");
        return 1;
    }
    printf("egl display via     : %s\n", how);

    EGLint major = 0, minor = 0;
    if (!eglInitialize(dpy, &major, &minor)) {
        printf("MALI PROBE RESULT: FAIL (eglInitialize: 0x%x)\n", eglGetError());
        print_maps("maps at failure");
        return 1;
    }
    printf("egl version         : %d.%d\n", major, minor);
    printf("EGL_VENDOR          : %s\n", or_null(eglQueryString(dpy, EGL_VENDOR)));
    printf("EGL_VERSION         : %s\n", or_null(eglQueryString(dpy, EGL_VERSION)));
    printf("egl apis            : %s\n", or_null(eglQueryString(dpy, EGL_CLIENT_APIS)));
    print_list("EGL_EXTENSIONS", eglQueryString(dpy, EGL_EXTENSIONS));

    if (!eglBindAPI(EGL_OPENGL_ES_API)) {
        printf("MALI PROBE RESULT: FAIL (eglBindAPI: 0x%x)\n", eglGetError());
        return 1;
    }

    const EGLint cfg_attr[] = {EGL_SURFACE_TYPE,    EGL_WINDOW_BIT,
                               EGL_RED_SIZE,        8,
                               EGL_GREEN_SIZE,      8,
                               EGL_BLUE_SIZE,       8,
                               EGL_ALPHA_SIZE,      0,
                               EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
                               EGL_NONE};
    EGLConfig config;
    EGLint n = 0;
    if (!eglChooseConfig(dpy, cfg_attr, &config, 1, &n) || n < 1) {
        printf("MALI PROBE RESULT: FAIL (no EGL config with a window surface)\n");
        return 1;
    }
    EGLint native_format = 0;
    eglGetConfigAttrib(dpy, config, EGL_NATIVE_VISUAL_ID, &native_format);
    printf("egl config          : %d matched, native visual 0x%08x\n", n, native_format);

    const EGLint ctx_attr[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    EGLContext ctx = eglCreateContext(dpy, config, EGL_NO_CONTEXT, ctx_attr);
    if (ctx == EGL_NO_CONTEXT) {
        printf("MALI PROBE RESULT: FAIL (eglCreateContext: 0x%x)\n", eglGetError());
        print_maps("maps at failure");
        return 1;
    }

    struct gbm_surface *surface = gbm_surface_create(
        gbm, 1920, 1080, native_format ? (uint32_t)native_format : GBM_FORMAT_XRGB8888,
        GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING);
    if (!surface) {
        printf("MALI PROBE RESULT: FAIL (gbm_surface_create)\n");
        return 1;
    }
    EGLSurface egl_surface =
        eglCreateWindowSurface(dpy, config, (EGLNativeWindowType)surface, NULL);
    if (egl_surface == EGL_NO_SURFACE) {
        printf("MALI PROBE RESULT: FAIL (eglCreateWindowSurface: 0x%x)\n", eglGetError());
        return 1;
    }
    if (!eglMakeCurrent(dpy, egl_surface, egl_surface, ctx)) {
        printf("MALI PROBE RESULT: FAIL (eglMakeCurrent: 0x%x)\n", eglGetError());
        print_maps("maps at failure");
        return 1;
    }

    const char *vendor = (const char *)glGetString(GL_VENDOR);
    const char *renderer = (const char *)glGetString(GL_RENDERER);
    printf("GL_VENDOR           : %s\n", or_null(vendor));
    printf("GL_RENDERER         : %s\n", or_null(renderer));
    printf("GL_VERSION          : %s\n", or_null((const char *)glGetString(GL_VERSION)));
    printf("GL_SL_VERSION       : %s\n",
           or_null((const char *)glGetString(GL_SHADING_LANGUAGE_VERSION)));
    print_list("GL_EXTENSIONS", (const char *)glGetString(GL_EXTENSIONS));

    // Render, finish, swap. Context creation alone can succeed on a driver
    // whose job control never reaches the GPU; a completed swap with a real
    // front buffer is the cheapest thing that cannot.
    glClearColor(0.1f, 0.2f, 0.3f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glFinish();
    int rendered = 1;
    if (!eglSwapBuffers(dpy, egl_surface)) {
        printf("eglSwapBuffers      : FAILED 0x%x\n", eglGetError());
        rendered = 0;
    } else {
        struct gbm_bo *bo = gbm_surface_lock_front_buffer(surface);
        if (!bo) {
            printf("front buffer        : none\n");
            rendered = 0;
        } else {
            printf("front buffer        : %ux%u stride %u format 0x%08x modifier 0x%llx\n",
                   gbm_bo_get_width(bo), gbm_bo_get_height(bo), gbm_bo_get_stride(bo),
                   gbm_bo_get_format(bo), (unsigned long long)gbm_bo_get_modifier(bo));
            gbm_surface_release_buffer(surface, bo);
        }
    }

    print_maps("mapped shared objects");

    const int software = renderer && (strstr(renderer, "llvmpipe") ||
                                      strstr(renderer, "softpipe") ||
                                      strstr(renderer, "swrast"));
    const int mali = renderer && (strstr(renderer, "Mali") || strstr(renderer, "mali"));

    // Teardown is part of the test: a driver that renders and then faults on
    // destroy is not usable by a process that stops and restarts playback.
    eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    eglDestroySurface(dpy, egl_surface);
    gbm_surface_destroy(surface);
    eglDestroyContext(dpy, ctx);
    eglTerminate(dpy);
    gbm_device_destroy(gbm);
    close(fd);
    printf("teardown            : clean\n");

    if (!rendered) {
        printf("MALI PROBE RESULT: FAIL (context came up, rendering did not)\n");
        return 1;
    }
    if (software) {
        printf("MALI PROBE RESULT: SOFTWARE (%s) - not a Mali pass\n", or_null(renderer));
        return 2;
    }
    if (!mali) {
        printf("MALI PROBE RESULT: UNKNOWN (renderer '%s' is neither software nor Mali)\n",
               or_null(renderer));
        return 3;
    }
    printf("MALI PROBE RESULT: PASS (hardware Mali)\n");
    return 0;
}
