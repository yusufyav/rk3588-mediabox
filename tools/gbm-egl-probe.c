// gbm-egl-probe - Gate MP2 GPU user-space smoke test.
//
// Kodi's GBM build needs three things this board had none of after Gate MP0:
// a GBM device on the KMS node, an EGL display on the GBM platform, and a
// GLES2 context. This proves all three before Kodi is built, so that a Kodi
// GUI failure later cannot be confused with a missing or broken GL stack.
//
// It deliberately does NOT test the video path. Kodi's DRMPRIME renderer puts
// decoded frames straight onto a DRM plane, so the GL stack never touches a
// video frame; what is proved here only bounds the GUI.
//
//   gbm-egl-probe [/dev/dri/card0]

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

int main(int argc, char **argv) {
    const char *node = argc > 1 ? argv[1] : "/dev/dri/card0";

    int fd = open(node, O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        printf("GBM/EGL RESULT: BLOCKED_GPU (open %s failed)\n", node);
        return 1;
    }
    printf("drm node            : %s (fd %d)\n", node, fd);

    struct gbm_device *gbm = gbm_create_device(fd);
    if (!gbm) {
        printf("GBM/EGL RESULT: BLOCKED_GPU (gbm_create_device failed)\n");
        return 1;
    }
    printf("gbm_device backend  : %s\n", or_null(gbm_device_get_backend_name(gbm)));

    // The GBM platform is what Kodi's WinSystemGbm uses. Going through
    // eglGetDisplay() instead would let EGL pick some other platform and prove
    // nothing about the path Kodi actually takes.
    PFNEGLGETPLATFORMDISPLAYEXTPROC get_platform_display =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    EGLDisplay dpy = get_platform_display
                         ? get_platform_display(EGL_PLATFORM_GBM_KHR, gbm, NULL)
                         : eglGetDisplay((EGLNativeDisplayType)gbm);
    if (dpy == EGL_NO_DISPLAY) {
        printf("GBM/EGL RESULT: BLOCKED_GPU (no EGL display on the GBM platform)\n");
        return 1;
    }

    EGLint major = 0, minor = 0;
    if (!eglInitialize(dpy, &major, &minor)) {
        printf("GBM/EGL RESULT: BLOCKED_GPU (eglInitialize: 0x%x)\n", eglGetError());
        return 1;
    }
    printf("egl version         : %d.%d\n", major, minor);
    printf("egl vendor          : %s\n", or_null(eglQueryString(dpy, EGL_VENDOR)));
    printf("egl apis            : %s\n", or_null(eglQueryString(dpy, EGL_CLIENT_APIS)));

    if (!eglBindAPI(EGL_OPENGL_ES_API)) {
        printf("GBM/EGL RESULT: BLOCKED_GPU (eglBindAPI(GLES): 0x%x)\n", eglGetError());
        return 1;
    }

    // Kodi asks for an 8-bit RGB(A) config with a window surface; matching that
    // here is the point, so a config that only Kodi would reject is not counted
    // as a pass.
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
        printf("GBM/EGL RESULT: BLOCKED_GPU (no EGL config with a window surface)\n");
        return 1;
    }
    EGLint native_format = 0;
    eglGetConfigAttrib(dpy, config, EGL_NATIVE_VISUAL_ID, &native_format);
    printf("egl configs matched : %d, native visual 0x%08x\n", n, native_format);

    const EGLint ctx_attr[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    EGLContext ctx = eglCreateContext(dpy, config, EGL_NO_CONTEXT, ctx_attr);
    if (ctx == EGL_NO_CONTEXT) {
        printf("GBM/EGL RESULT: BLOCKED_GPU (eglCreateContext: 0x%x)\n", eglGetError());
        return 1;
    }

    // A real GBM surface in the config's own native format, then make it
    // current: a context that cannot be bound to a scanout-capable surface
    // would still fail in Kodi.
    struct gbm_surface *surface = gbm_surface_create(
        gbm, 1920, 1080, native_format ? (uint32_t)native_format : GBM_FORMAT_XRGB8888,
        GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING);
    if (!surface) {
        printf("GBM/EGL RESULT: BLOCKED_GPU (gbm_surface_create failed)\n");
        return 1;
    }
    EGLSurface egl_surface = eglCreateWindowSurface(dpy, config,
                                                    (EGLNativeWindowType)surface, NULL);
    if (egl_surface == EGL_NO_SURFACE) {
        printf("GBM/EGL RESULT: BLOCKED_GPU (eglCreateWindowSurface: 0x%x)\n", eglGetError());
        return 1;
    }
    if (!eglMakeCurrent(dpy, egl_surface, egl_surface, ctx)) {
        printf("GBM/EGL RESULT: BLOCKED_GPU (eglMakeCurrent: 0x%x)\n", eglGetError());
        return 1;
    }

    printf("gl vendor           : %s\n", or_null((const char *)glGetString(GL_VENDOR)));
    printf("gl renderer         : %s\n", or_null((const char *)glGetString(GL_RENDERER)));
    printf("gl version          : %s\n", or_null((const char *)glGetString(GL_VERSION)));
    printf("glsl version        : %s\n", or_null((const char *)glGetString(GL_SHADING_LANGUAGE_VERSION)));

    // Draw something and swap, so the result covers rendering rather than only
    // context creation.
    glClearColor(0.1f, 0.2f, 0.3f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glFinish();
    if (!eglSwapBuffers(dpy, egl_surface)) {
        printf("GBM/EGL RESULT: PARTIAL (rendering worked, eglSwapBuffers: 0x%x)\n",
               eglGetError());
        return 1;
    }
    struct gbm_bo *bo = gbm_surface_lock_front_buffer(surface);
    if (!bo) {
        printf("GBM/EGL RESULT: PARTIAL (swap worked, no front buffer to scan out)\n");
        return 1;
    }
    printf("front buffer        : %ux%u stride %u format 0x%08x modifier 0x%llx\n",
           gbm_bo_get_width(bo), gbm_bo_get_height(bo), gbm_bo_get_stride(bo),
           gbm_bo_get_format(bo), (unsigned long long)gbm_bo_get_modifier(bo));
    gbm_surface_release_buffer(surface, bo);

    const char *renderer = (const char *)glGetString(GL_RENDERER);
    const int software = renderer && (strstr(renderer, "llvmpipe") ||
                                      strstr(renderer, "softpipe") ||
                                      strstr(renderer, "swrast"));
    printf("acceleration        : %s\n",
           software ? "SOFTWARE (llvmpipe/swrast)" : "hardware or unknown");

    eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    eglDestroySurface(dpy, egl_surface);
    gbm_surface_destroy(surface);
    eglDestroyContext(dpy, ctx);
    eglTerminate(dpy);
    gbm_device_destroy(gbm);
    close(fd);

    printf("GBM/EGL RESULT: PASS%s\n", software ? " (software rendering)" : "");
    return 0;
}
