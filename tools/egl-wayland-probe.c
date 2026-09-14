/* Does an ordinary Wayland client get an EGL display on this box?
 *
 * Written because the answer could not be got out of anything else. A toolkit
 * that fails here fails silently — the window is simply never mapped — and mpv
 * says only "Failed initializing any suitable GPU context". Neither says which
 * call refused, or what EGL's own error was.
 *
 * Deliberately minimal: Wayland, EGL, and nothing else. No GL, no surface, no
 * toolkit. It stops at eglInitialize, because that is where the failure is.
 *
 *   aarch64-linux-gnu-gcc --sysroot=<sysroot> -o egl-wayland-probe \
 *       tools/egl-wayland-probe.c -lwayland-client -lEGL
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <unistd.h>
#include <gbm.h>
#include <wayland-client.h>
#include <EGL/egl.h>
#include <EGL/eglext.h>

static const char *egl_error(EGLint code) {
    switch (code) {
    case EGL_SUCCESS: return "EGL_SUCCESS";
    case EGL_NOT_INITIALIZED: return "EGL_NOT_INITIALIZED";
    case EGL_BAD_ACCESS: return "EGL_BAD_ACCESS";
    case EGL_BAD_ALLOC: return "EGL_BAD_ALLOC";
    case EGL_BAD_ATTRIBUTE: return "EGL_BAD_ATTRIBUTE";
    case EGL_BAD_CONFIG: return "EGL_BAD_CONFIG";
    case EGL_BAD_CONTEXT: return "EGL_BAD_CONTEXT";
    case EGL_BAD_CURRENT_SURFACE: return "EGL_BAD_CURRENT_SURFACE";
    case EGL_BAD_DISPLAY: return "EGL_BAD_DISPLAY";
    case EGL_BAD_MATCH: return "EGL_BAD_MATCH";
    case EGL_BAD_NATIVE_PIXMAP: return "EGL_BAD_NATIVE_PIXMAP";
    case EGL_BAD_NATIVE_WINDOW: return "EGL_BAD_NATIVE_WINDOW";
    case EGL_BAD_PARAMETER: return "EGL_BAD_PARAMETER";
    case EGL_BAD_SURFACE: return "EGL_BAD_SURFACE";
    case EGL_CONTEXT_LOST: return "EGL_CONTEXT_LOST";
    default: return "unknown";
    }
}

/* Which file a soname actually resolved to, which is the whole question when
 * two packages provide the same one. */
static void whence(const char *soname) {
    void *handle = dlopen(soname, RTLD_NOW | RTLD_NOLOAD);
    if (!handle) {
        handle = dlopen(soname, RTLD_NOW);
        if (!handle) {
            printf("  %-24s not loadable: %s\n", soname, dlerror());
            return;
        }
    }
    /* A symbol that belongs to this library and to no other, so the path
     * reported is the file that actually answered for the soname. Looking up
     * something it merely re-exports reports whichever library defines it,
     * which is how an earlier run claimed libwayland-egl lived in
     * libwayland-client. */
    const char *witness =
        !strcmp(soname, "libEGL.so.1") ? "eglInitialize"
        : !strcmp(soname, "libwayland-client.so.0") ? "wl_display_connect"
        : !strcmp(soname, "libwayland-egl.so.1") ? "wl_egl_window_create"
        : NULL;

    Dl_info info;
    void *sym = witness ? dlsym(handle, witness) : NULL;
    if (sym && dladdr(sym, &info) && info.dli_fname) {
        printf("  %-24s %s\n", soname, info.dli_fname);
    } else {
        printf("  %-24s loaded, origin unknown\n", soname);
    }
}

/* The same driver, the same process, the other platform.
 *
 * The compositor uses GBM and works, but the compositor is not a client. This
 * asks whether an ordinary process can initialise this driver at all, so that
 * a Wayland failure can be attributed to the Wayland path rather than to the
 * driver being unusable outside the compositor. */
static void probe_gbm(void) {
    printf("\n== control: the same driver on the GBM platform\n");
    int fd = open("/dev/dri/renderD128", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        printf("  /dev/dri/renderD128: cannot open\n");
        return;
    }
    struct gbm_device *gbm = gbm_create_device(fd);
    if (!gbm) {
        printf("  gbm_create_device FAILED\n");
        close(fd);
        return;
    }
    PFNEGLGETPLATFORMDISPLAYEXTPROC get_platform_display =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    EGLDisplay dpy = get_platform_display
        ? get_platform_display(EGL_PLATFORM_GBM_KHR, gbm, NULL)
        : EGL_NO_DISPLAY;
    printf("  display: %p  error: %s\n", (void *)dpy, egl_error(eglGetError()));
    if (dpy == EGL_NO_DISPLAY) {
        return;
    }
    EGLint major = 0, minor = 0;
    EGLBoolean ok = eglInitialize(dpy, &major, &minor);
    printf("  eglInitialize returned %d  error: %s\n", ok, egl_error(eglGetError()));
    if (ok) {
        printf("  EGL_VERSION = %s\n", eglQueryString(dpy, EGL_VERSION));
        printf("  EGL_VENDOR  = %s\n", eglQueryString(dpy, EGL_VENDOR));
        EGLint count = 0;
        eglGetConfigs(dpy, NULL, 0, &count);
        printf("  configs: %d\n", count);
    }
}

int main(void) {
    printf("== libraries as the loader resolves them\n");
    whence("libEGL.so.1");
    whence("libwayland-client.so.0");
    whence("libwayland-egl.so.1");

    printf("\n== client extensions (before any display)\n");
    const char *client_ext = eglQueryString(EGL_NO_DISPLAY, EGL_EXTENSIONS);
    printf("  %s\n", client_ext ? client_ext : "(none reported)");

    int has_wayland_ext = client_ext && strstr(client_ext, "EGL_EXT_platform_wayland");
    printf("  EGL_EXT_platform_wayland advertised: %s\n", has_wayland_ext ? "yes" : "NO");

    probe_gbm();

    printf("\n== wayland connection\n");
    struct wl_display *wl = wl_display_connect(NULL);
    if (!wl) {
        printf("  wl_display_connect FAILED\n");
        return 2;
    }
    printf("  wl_display_connect ok\n");

    printf("\n== eglGetPlatformDisplayEXT(EGL_PLATFORM_WAYLAND_KHR)\n");
    PFNEGLGETPLATFORMDISPLAYEXTPROC get_platform_display =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    printf("  eglGetProcAddress: %s\n", get_platform_display ? "resolved" : "NULL");

    EGLDisplay dpy = EGL_NO_DISPLAY;
    if (get_platform_display) {
        dpy = get_platform_display(EGL_PLATFORM_WAYLAND_KHR, wl, NULL);
        printf("  display: %p  error: %s\n", (void *)dpy, egl_error(eglGetError()));
    }

    if (dpy == EGL_NO_DISPLAY) {
        printf("\n== falling back to eglGetDisplay(wl_display)\n");
        dpy = eglGetDisplay((EGLNativeDisplayType)wl);
        printf("  display: %p  error: %s\n", (void *)dpy, egl_error(eglGetError()));
    }

    if (dpy == EGL_NO_DISPLAY) {
        printf("\nRESULT: no EGL display for the Wayland platform\n");
        return 3;
    }

    printf("\n== eglInitialize\n");
    EGLint major = 0, minor = 0;
    EGLBoolean ok = eglInitialize(dpy, &major, &minor);
    printf("  returned %d  error: %s\n", ok, egl_error(eglGetError()));
    if (!ok) {
        printf("\nRESULT: display obtained, eglInitialize refused it\n");
        return 4;
    }

    printf("  EGL_VERSION = %s\n", eglQueryString(dpy, EGL_VERSION));
    printf("  EGL_VENDOR  = %s\n", eglQueryString(dpy, EGL_VENDOR));
    printf("  EGL_CLIENT_APIS = %s\n", eglQueryString(dpy, EGL_CLIENT_APIS));

    const char *ext = eglQueryString(dpy, EGL_EXTENSIONS);
    printf("  EGL_WL_bind_wayland_display: %s\n",
           ext && strstr(ext, "EGL_WL_bind_wayland_display") ? "yes" : "no");

    EGLint count = 0;
    eglGetConfigs(dpy, NULL, 0, &count);
    printf("  configs: %d  error: %s\n", count, egl_error(eglGetError()));

    printf("\nRESULT: the Wayland platform works\n");
    return 0;
}
