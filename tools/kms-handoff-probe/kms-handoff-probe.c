#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include <drm_fourcc.h>
#include <drm_mode.h>
#include <xf86drm.h>
#include <xf86drmMode.h>

#define DEFAULT_HOLD_SECONDS 2
#define MAX_HOLD_SECONDS 10
#define MAX_CARD_INDEX 15

enum run_mode {
    MODE_NONE,
    MODE_INSPECT,
    MODE_ACQUIRE_ONLY,
    MODE_SCANOUT_TEST,
};

struct property {
    uint32_t id;
    uint64_t value;
    bool found;
};

struct topology {
    int fd;
    char card[64];
    char driver[128];
    bool atomic;
    bool universal_planes;
    bool is_master;

    uint32_t connector_id;
    uint32_t connector_type_id;
    uint32_t encoder_id;
    uint32_t crtc_id;
    int crtc_index;
    drmModeModeInfo mode;
    bool mode_valid;

    uint32_t plane_id;
    uint64_t plane_type;
};

struct dumb_buffer {
    uint32_t handle;
    uint32_t pitch;
    uint64_t size;
    uint32_t fb_id;
    void *map;
};

struct scanout_state {
    struct topology topo;
    struct dumb_buffer buffer;
    uint32_t mode_blob_id;
    bool master_acquired;
    bool scanout_committed;
};

static volatile sig_atomic_t stop_requested;

static void signal_handler(int signal_number)
{
    (void)signal_number;
    stop_requested = 1;
}

static void install_signal_handlers(void)
{
    struct sigaction action;

    memset(&action, 0, sizeof(action));
    action.sa_handler = signal_handler;
    sigemptyset(&action.sa_mask);
    (void)sigaction(SIGINT, &action, NULL);
    (void)sigaction(SIGTERM, &action, NULL);
    (void)sigaction(SIGHUP, &action, NULL);
    (void)signal(SIGPIPE, SIG_IGN);
}

static int negative_errno(int result)
{
    if (result >= 0)
        return result;
    return errno ? -errno : result;
}

static double mode_refresh(const drmModeModeInfo *mode)
{
    double refresh;

    if (!mode->htotal || !mode->vtotal)
        return 0.0;
    refresh = (double)mode->clock * 1000.0 /
              ((double)mode->htotal * (double)mode->vtotal);
    if (mode->flags & DRM_MODE_FLAG_INTERLACE)
        refresh *= 2.0;
    if (mode->flags & DRM_MODE_FLAG_DBLSCAN)
        refresh /= 2.0;
    if (mode->vscan > 1)
        refresh /= mode->vscan;
    return refresh;
}

static void fourcc_to_string(uint32_t format, char output[5])
{
    output[0] = (char)(format & 0xffU);
    output[1] = (char)((format >> 8) & 0xffU);
    output[2] = (char)((format >> 16) & 0xffU);
    output[3] = (char)((format >> 24) & 0xffU);
    output[4] = '\0';
}

static struct property get_property(int fd, uint32_t object_id,
                                    uint32_t object_type, const char *name)
{
    struct property result = {0};
    drmModeObjectProperties *properties;
    uint32_t i;

    properties = drmModeObjectGetProperties(fd, object_id, object_type);
    if (!properties)
        return result;

    for (i = 0; i < properties->count_props; ++i) {
        drmModePropertyRes *property = drmModeGetProperty(fd, properties->props[i]);

        if (!property)
            continue;
        if (strcmp(property->name, name) == 0) {
            result.id = property->prop_id;
            result.value = properties->prop_values[i];
            result.found = true;
            drmModeFreeProperty(property);
            break;
        }
        drmModeFreeProperty(property);
    }
    drmModeFreeObjectProperties(properties);
    return result;
}

static const char *plane_type_name(uint64_t type)
{
    switch (type) {
    case DRM_PLANE_TYPE_OVERLAY:
        return "Overlay";
    case DRM_PLANE_TYPE_PRIMARY:
        return "Primary";
    case DRM_PLANE_TYPE_CURSOR:
        return "Cursor";
    default:
        return "Unknown";
    }
}

static bool plane_has_format(const drmModePlane *plane, uint32_t wanted)
{
    uint32_t i;

    for (i = 0; i < plane->count_formats; ++i) {
        if (plane->formats[i] == wanted)
            return true;
    }
    return false;
}

static int find_crtc_index(const drmModeRes *resources, uint32_t crtc_id)
{
    int i;

    for (i = 0; i < resources->count_crtcs; ++i) {
        if (resources->crtcs[i] == crtc_id)
            return i;
    }
    return -1;
}

static uint32_t connector_crtc_id(int fd, const drmModeConnector *connector)
{
    struct property crtc_property =
        get_property(fd, connector->connector_id, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID");

    if (crtc_property.found && crtc_property.value)
        return (uint32_t)crtc_property.value;

    if (connector->encoder_id) {
        drmModeEncoder *encoder = drmModeGetEncoder(fd, connector->encoder_id);
        uint32_t crtc_id = 0;

        if (encoder) {
            crtc_id = encoder->crtc_id;
            drmModeFreeEncoder(encoder);
        }
        return crtc_id;
    }
    return 0;
}

static int enable_client_caps(int fd, bool *universal, bool *atomic)
{
    int result;

    *universal = false;
    *atomic = false;

    result = drmSetClientCap(fd, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1);
    if (result != 0)
        return negative_errno(result);
    *universal = true;

    result = drmSetClientCap(fd, DRM_CLIENT_CAP_ATOMIC, 1);
    if (result == 0)
        *atomic = true;
    return 0;
}

static int select_primary_plane(struct topology *topology, bool print_formats)
{
    drmModePlaneRes *plane_resources;
    uint32_t i;
    int status = -ENOENT;

    plane_resources = drmModeGetPlaneResources(topology->fd);
    if (!plane_resources)
        return -errno;

    for (i = 0; i < plane_resources->count_planes; ++i) {
        drmModePlane *plane = drmModeGetPlane(topology->fd, plane_resources->planes[i]);
        struct property type;
        bool compatible;
        bool xrgb;

        if (!plane)
            continue;
        type = get_property(topology->fd, plane->plane_id,
                            DRM_MODE_OBJECT_PLANE, "type");
        compatible = topology->crtc_index >= 0 &&
                     (plane->possible_crtcs & (1U << topology->crtc_index));
        xrgb = plane_has_format(plane, DRM_FORMAT_XRGB8888);

        if (print_formats && compatible && type.found &&
            type.value == DRM_PLANE_TYPE_PRIMARY) {
            uint32_t f;

            printf("primary plane       : id=%u possible_crtcs=0x%x current_crtc=%u current_fb=%u\n",
                   plane->plane_id, plane->possible_crtcs, plane->crtc_id, plane->fb_id);
            printf("plane formats       :");
            for (f = 0; f < plane->count_formats; ++f) {
                char name[5];

                fourcc_to_string(plane->formats[f], name);
                printf(" %s", name);
            }
            putchar('\n');
        }

        if (status != 0 && compatible && xrgb && type.found &&
            type.value == DRM_PLANE_TYPE_PRIMARY) {
            topology->plane_id = plane->plane_id;
            topology->plane_type = type.value;
            status = 0;
        }
        drmModeFreePlane(plane);
    }
    drmModeFreePlaneResources(plane_resources);
    return status;
}

static int inspect_open_card(const char *path, struct topology *topology)
{
    drmVersionPtr version;
    drmModeRes *resources;
    int cap_status;
    int i;

    memset(topology, 0, sizeof(*topology));
    topology->fd = open(path, O_RDWR | O_CLOEXEC | O_NONBLOCK);
    if (topology->fd < 0)
        return -errno;
    snprintf(topology->card, sizeof(topology->card), "%s", path);

    version = drmGetVersion(topology->fd);
    if (!version) {
        close(topology->fd);
        topology->fd = -1;
        return -ENODEV;
    }
    snprintf(topology->driver, sizeof(topology->driver), "%.*s",
             version->name_len, version->name);
    drmFreeVersion(version);

    cap_status = enable_client_caps(topology->fd, &topology->universal_planes,
                                    &topology->atomic);
    if (cap_status != 0) {
        close(topology->fd);
        topology->fd = -1;
        return cap_status;
    }

    resources = drmModeGetResources(topology->fd);
    if (!resources) {
        close(topology->fd);
        topology->fd = -1;
        return -errno;
    }

    for (i = 0; i < resources->count_connectors; ++i) {
        drmModeConnector *connector =
            drmModeGetConnector(topology->fd, resources->connectors[i]);

        if (!connector)
            continue;
        if (connector->connector_type == DRM_MODE_CONNECTOR_HDMIA &&
            connector->connection == DRM_MODE_CONNECTED) {
            drmModeCrtc *crtc;

            topology->connector_id = connector->connector_id;
            topology->connector_type_id = connector->connector_type_id;
            topology->encoder_id = connector->encoder_id;
            topology->crtc_id = connector_crtc_id(topology->fd, connector);
            topology->crtc_index = find_crtc_index(resources, topology->crtc_id);
            crtc = topology->crtc_id ? drmModeGetCrtc(topology->fd, topology->crtc_id) : NULL;
            if (crtc) {
                if (crtc->mode_valid) {
                    topology->mode = crtc->mode;
                    topology->mode_valid = true;
                }
                drmModeFreeCrtc(crtc);
            }
            drmModeFreeConnector(connector);
            break;
        }
        drmModeFreeConnector(connector);
    }
    drmModeFreeResources(resources);

    if (!topology->connector_id) {
        close(topology->fd);
        topology->fd = -1;
        return -ENOTCONN;
    }
    topology->is_master = drmIsMaster(topology->fd) == 1;
    return 0;
}

static int discover_topology(struct topology *topology)
{
    char path[64];
    int index;
    int best_error = -ENODEV;

    for (index = 0; index <= MAX_CARD_INDEX; ++index) {
        int result;

        snprintf(path, sizeof(path), "/dev/dri/card%d", index);
        result = inspect_open_card(path, topology);
        if (result == 0)
            return 0;
        if (result != -ENOENT && result != -ENODEV && result != -ENOTCONN)
            best_error = result;
    }
    return best_error;
}

static void close_topology(struct topology *topology)
{
    if (topology->fd >= 0) {
        close(topology->fd);
        topology->fd = -1;
    }
}

static int print_connector_details(const struct topology *topology)
{
    drmModeConnector *connector;
    const char *state;

    connector = drmModeGetConnector(topology->fd, topology->connector_id);
    if (!connector)
        return -errno;
    switch (connector->connection) {
    case DRM_MODE_CONNECTED:
        state = "connected";
        break;
    case DRM_MODE_DISCONNECTED:
        state = "disconnected";
        break;
    default:
        state = "unknown";
        break;
    }
    printf("DRM card            : %s\n", topology->card);
    printf("driver              : %s\n", topology->driver);
    printf("connector           : HDMI-A-%u\n", topology->connector_type_id);
    printf("connector id/state  : %u / %s\n", topology->connector_id, state);
    printf("encoder id          : %u\n", topology->encoder_id);
    printf("CRTC id/index       : %u / %d\n", topology->crtc_id, topology->crtc_index);
    if (topology->mode_valid) {
        printf("current mode         : %s %ux%u\n", topology->mode.name,
               topology->mode.hdisplay, topology->mode.vdisplay);
        printf("current refresh      : %.3f Hz\n", mode_refresh(&topology->mode));
    } else {
        printf("current mode         : inactive/unavailable\n");
        printf("current refresh      : unavailable\n");
    }
    printf("universal planes     : %s\n", topology->universal_planes ? "yes" : "no");
    printf("atomic capability    : %s\n", topology->atomic ? "yes" : "no");
    printf("this fd DRM master   : %s (passive drmIsMaster; no master request)\n",
           topology->is_master ? "yes" : "no");
    drmModeFreeConnector(connector);
    return 0;
}

static int run_inspect(void)
{
    struct topology topology;
    int result;

    result = discover_topology(&topology);
    if (result != 0) {
        fprintf(stderr, "INSPECT RESULT: FAIL (display DRM card/connected HDMI discovery: %s)\n",
                strerror(-result));
        return EXIT_FAILURE;
    }
    result = print_connector_details(&topology);
    if (result == 0)
        result = select_primary_plane(&topology, true);
    if (result != 0) {
        fprintf(stderr, "INSPECT RESULT: FAIL (usable XRGB8888 primary plane: %s)\n",
                strerror(-result));
        close_topology(&topology);
        return EXIT_FAILURE;
    }
    printf("selected plane      : %u (%s)\n", topology.plane_id,
           plane_type_name(topology.plane_type));
    printf("INSPECT RESULT: PASS (side-effect-free; no modeset, no master request)\n");
    close_topology(&topology);
    return EXIT_SUCCESS;
}

static uint64_t monotonic_ns(void)
{
    struct timespec time_value;

    if (clock_gettime(CLOCK_MONOTONIC, &time_value) != 0)
        return 0;
    return (uint64_t)time_value.tv_sec * UINT64_C(1000000000) +
           (uint64_t)time_value.tv_nsec;
}

static int acquire_master(struct scanout_state *state)
{
    int result = drmSetMaster(state->topo.fd);

    if (result != 0)
        return negative_errno(result);
    if (drmIsMaster(state->topo.fd) != 1)
        return -EACCES;
    state->master_acquired = true;
    printf("event=DRM_MASTER_ACQUIRED monotonic_ns=%" PRIu64 " card=%s\n",
           monotonic_ns(), state->topo.card);
    fflush(stdout);
    return 0;
}

static void finite_hold(unsigned int seconds)
{
    struct timespec sleep_time = {.tv_sec = 0, .tv_nsec = 100000000L};
    uint64_t deadline = monotonic_ns() + (uint64_t)seconds * UINT64_C(1000000000);

    while (!stop_requested && monotonic_ns() < deadline)
        (void)nanosleep(&sleep_time, NULL);
}

static int create_dumb_buffer(struct scanout_state *state)
{
    struct drm_mode_create_dumb create_request;
    struct drm_mode_map_dumb map_request;
    uint32_t handles[4] = {0};
    uint32_t pitches[4] = {0};
    uint32_t offsets[4] = {0};
    uint32_t x;
    uint32_t y;
    int result;

    memset(&create_request, 0, sizeof(create_request));
    create_request.width = state->topo.mode.hdisplay;
    create_request.height = state->topo.mode.vdisplay;
    create_request.bpp = 32;
    result = drmIoctl(state->topo.fd, DRM_IOCTL_MODE_CREATE_DUMB, &create_request);
    if (result != 0)
        return negative_errno(result);

    state->buffer.handle = create_request.handle;
    state->buffer.pitch = create_request.pitch;
    state->buffer.size = create_request.size;

    handles[0] = state->buffer.handle;
    pitches[0] = state->buffer.pitch;
    result = drmModeAddFB2(state->topo.fd, create_request.width, create_request.height,
                           DRM_FORMAT_XRGB8888, handles, pitches, offsets,
                           &state->buffer.fb_id, 0);
    if (result != 0)
        return negative_errno(result);

    memset(&map_request, 0, sizeof(map_request));
    map_request.handle = state->buffer.handle;
    result = drmIoctl(state->topo.fd, DRM_IOCTL_MODE_MAP_DUMB, &map_request);
    if (result != 0)
        return negative_errno(result);

    state->buffer.map = mmap(NULL, state->buffer.size, PROT_READ | PROT_WRITE,
                             MAP_SHARED, state->topo.fd, map_request.offset);
    if (state->buffer.map == MAP_FAILED) {
        state->buffer.map = NULL;
        return -errno;
    }

    for (y = 0; y < create_request.height; ++y) {
        uint32_t *row = (uint32_t *)((uint8_t *)state->buffer.map +
                                     (uint64_t)y * state->buffer.pitch);
        for (x = 0; x < create_request.width; ++x) {
            uint32_t band = (x * 8U) / create_request.width;
            static const uint32_t colors[8] = {
                0x00b0b0b0U, 0x00b0b000U, 0x0000b0b0U, 0x0000b000U,
                0x00b000b0U, 0x00b00000U, 0x000000b0U, 0x00303030U,
            };
            uint32_t checker = ((x / 64U) ^ (y / 64U)) & 1U;
            row[x] = colors[band] + (checker ? 0x00080808U : 0U);
        }
    }
    (void)msync(state->buffer.map, state->buffer.size, MS_SYNC);
    printf("framebuffer_created : fb=%u handle=%u %ux%u pitch=%u size=%" PRIu64
           " format=XR24\n",
           state->buffer.fb_id, state->buffer.handle, create_request.width,
           create_request.height, state->buffer.pitch, state->buffer.size);
    return 0;
}

static int atomic_add(drmModeAtomicReq *request, uint32_t object_id,
                      uint32_t property_id, uint64_t value)
{
    if (!property_id)
        return -EINVAL;
    if (drmModeAtomicAddProperty(request, object_id, property_id, value) < 0)
        return -errno;
    return 0;
}

static int add_required_property(drmModeAtomicReq *request, int fd,
                                 uint32_t object_id, uint32_t object_type,
                                 const char *name, uint64_t value)
{
    struct property property = get_property(fd, object_id, object_type, name);

    if (!property.found) {
        fprintf(stderr, "missing required property object=%u name=%s\n", object_id, name);
        return -ENOENT;
    }
    return atomic_add(request, object_id, property.id, value);
}

static int disable_other_planes(drmModeAtomicReq *request,
                                const struct scanout_state *state)
{
    drmModePlaneRes *plane_resources;
    uint32_t i;
    int result = 0;

    plane_resources = drmModeGetPlaneResources(state->topo.fd);
    if (!plane_resources)
        return -errno;
    for (i = 0; i < plane_resources->count_planes; ++i) {
        drmModePlane *plane = drmModeGetPlane(state->topo.fd, plane_resources->planes[i]);

        if (!plane)
            continue;
        if (plane->plane_id != state->topo.plane_id &&
            plane->crtc_id == state->topo.crtc_id) {
            result = add_required_property(request, state->topo.fd, plane->plane_id,
                                           DRM_MODE_OBJECT_PLANE, "FB_ID", 0);
            if (result == 0)
                result = add_required_property(request, state->topo.fd, plane->plane_id,
                                               DRM_MODE_OBJECT_PLANE, "CRTC_ID", 0);
        }
        drmModeFreePlane(plane);
        if (result != 0)
            break;
    }
    drmModeFreePlaneResources(plane_resources);
    return result;
}

static int build_scanout_request(struct scanout_state *state,
                                 drmModeAtomicReq *request)
{
    int result;

    result = add_required_property(request, state->topo.fd, state->topo.connector_id,
                                   DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID",
                                   state->topo.crtc_id);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.crtc_id,
                                       DRM_MODE_OBJECT_CRTC, "MODE_ID",
                                       state->mode_blob_id);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.crtc_id,
                                       DRM_MODE_OBJECT_CRTC, "ACTIVE", 1);
    if (result == 0)
        result = disable_other_planes(request, state);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.plane_id,
                                       DRM_MODE_OBJECT_PLANE, "FB_ID",
                                       state->buffer.fb_id);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.plane_id,
                                       DRM_MODE_OBJECT_PLANE, "CRTC_ID",
                                       state->topo.crtc_id);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.plane_id,
                                       DRM_MODE_OBJECT_PLANE, "SRC_X", 0);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.plane_id,
                                       DRM_MODE_OBJECT_PLANE, "SRC_Y", 0);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.plane_id,
                                       DRM_MODE_OBJECT_PLANE, "SRC_W",
                                       (uint64_t)state->topo.mode.hdisplay << 16);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.plane_id,
                                       DRM_MODE_OBJECT_PLANE, "SRC_H",
                                       (uint64_t)state->topo.mode.vdisplay << 16);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.plane_id,
                                       DRM_MODE_OBJECT_PLANE, "CRTC_X", 0);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.plane_id,
                                       DRM_MODE_OBJECT_PLANE, "CRTC_Y", 0);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.plane_id,
                                       DRM_MODE_OBJECT_PLANE, "CRTC_W",
                                       state->topo.mode.hdisplay);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.plane_id,
                                       DRM_MODE_OBJECT_PLANE, "CRTC_H",
                                       state->topo.mode.vdisplay);
    return result;
}

static int atomic_scanout(struct scanout_state *state)
{
    drmModeAtomicReq *request;
    int result;

    result = drmModeCreatePropertyBlob(state->topo.fd, &state->topo.mode,
                                       sizeof(state->topo.mode),
                                       &state->mode_blob_id);
    if (result != 0)
        return negative_errno(result);

    request = drmModeAtomicAlloc();
    if (!request)
        return -ENOMEM;
    result = build_scanout_request(state, request);
    if (result == 0) {
        result = drmModeAtomicCommit(state->topo.fd, request,
                                     DRM_MODE_ATOMIC_TEST_ONLY |
                                         DRM_MODE_ATOMIC_ALLOW_MODESET,
                                     NULL);
        result = negative_errno(result);
    }
    drmModeAtomicFree(request);
    if (result != 0)
        return result;
    printf("atomic_test_only     : success\n");

    request = drmModeAtomicAlloc();
    if (!request)
        return -ENOMEM;
    result = build_scanout_request(state, request);
    if (result == 0) {
        result = drmModeAtomicCommit(state->topo.fd, request,
                                     DRM_MODE_ATOMIC_ALLOW_MODESET, NULL);
        result = negative_errno(result);
    }
    drmModeAtomicFree(request);
    if (result != 0)
        return result;
    state->scanout_committed = true;
    printf("event=ATOMIC_SCANOUT_COMMITTED monotonic_ns=%" PRIu64
           " connector=%u crtc=%u plane=%u fb=%u mode=%s\n",
           monotonic_ns(), state->topo.connector_id, state->topo.crtc_id,
           state->topo.plane_id, state->buffer.fb_id, state->topo.mode.name);
    fflush(stdout);
    return 0;
}

static int detach_scanout(struct scanout_state *state)
{
    drmModeAtomicReq *request;
    int result;

    if (!state->scanout_committed)
        return 0;
    request = drmModeAtomicAlloc();
    if (!request)
        return -ENOMEM;
    result = add_required_property(request, state->topo.fd, state->topo.plane_id,
                                   DRM_MODE_OBJECT_PLANE, "FB_ID", 0);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.plane_id,
                                       DRM_MODE_OBJECT_PLANE, "CRTC_ID", 0);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.connector_id,
                                       DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID", 0);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.crtc_id,
                                       DRM_MODE_OBJECT_CRTC, "ACTIVE", 0);
    if (result == 0)
        result = add_required_property(request, state->topo.fd, state->topo.crtc_id,
                                       DRM_MODE_OBJECT_CRTC, "MODE_ID", 0);
    if (result == 0) {
        result = drmModeAtomicCommit(state->topo.fd, request,
                                     DRM_MODE_ATOMIC_ALLOW_MODESET, NULL);
        result = negative_errno(result);
    }
    drmModeAtomicFree(request);
    if (result == 0)
        state->scanout_committed = false;
    return result;
}

static int cleanup_scanout(struct scanout_state *state)
{
    int first_error = 0;
    int result;

    result = detach_scanout(state);
    if (result != 0) {
        fprintf(stderr, "cleanup warning: atomic detach failed: %s\n", strerror(-result));
        first_error = result;
    }
    if (state->buffer.map) {
        if (munmap(state->buffer.map, state->buffer.size) != 0 && first_error == 0)
            first_error = -errno;
        state->buffer.map = NULL;
    }
    if (state->buffer.fb_id) {
        result = drmModeRmFB(state->topo.fd, state->buffer.fb_id);
        if (result != 0 && first_error == 0)
            first_error = negative_errno(result);
        state->buffer.fb_id = 0;
    }
    if (state->buffer.handle) {
        struct drm_mode_destroy_dumb destroy_request;

        memset(&destroy_request, 0, sizeof(destroy_request));
        destroy_request.handle = state->buffer.handle;
        result = drmIoctl(state->topo.fd, DRM_IOCTL_MODE_DESTROY_DUMB,
                          &destroy_request);
        if (result != 0 && first_error == 0)
            first_error = negative_errno(result);
        state->buffer.handle = 0;
    }
    if (state->mode_blob_id) {
        result = drmModeDestroyPropertyBlob(state->topo.fd, state->mode_blob_id);
        if (result != 0 && first_error == 0)
            first_error = negative_errno(result);
        state->mode_blob_id = 0;
    }
    if (state->master_acquired) {
        result = drmDropMaster(state->topo.fd);
        if (result != 0 && first_error == 0)
            first_error = negative_errno(result);
        state->master_acquired = false;
        printf("event=DRM_MASTER_RELEASED monotonic_ns=%" PRIu64 "\n", monotonic_ns());
    }
    close_topology(&state->topo);
    return first_error;
}

static int prepare_active_topology(struct scanout_state *state)
{
    int result;

    memset(state, 0, sizeof(*state));
    state->topo.fd = -1;
    result = discover_topology(&state->topo);
    if (result != 0)
        return result;
    if (!state->topo.atomic)
        return -EOPNOTSUPP;
    if (!state->topo.crtc_id || state->topo.crtc_index < 0 ||
        !state->topo.mode_valid)
        return -ENODATA;
    result = select_primary_plane(&state->topo, false);
    if (result != 0)
        return result;
    return 0;
}

static int run_acquire_only(unsigned int hold_seconds)
{
    struct scanout_state state;
    int result;
    int cleanup_result;

    memset(&state, 0, sizeof(state));
    state.topo.fd = -1;
    result = discover_topology(&state.topo);
    if (result != 0) {
        fprintf(stderr, "ACQUIRE-ONLY RESULT: FAIL (discovery: %s)\n", strerror(-result));
        return EXIT_FAILURE;
    }
    printf("event=PROBE_STARTED monotonic_ns=%" PRIu64 " mode=acquire-only\n",
           monotonic_ns());
    result = acquire_master(&state);
    if (result == 0)
        finite_hold(hold_seconds);
    cleanup_result = cleanup_scanout(&state);
    if (result != 0) {
        fprintf(stderr, "ACQUIRE-ONLY RESULT: FAIL (drmSetMaster/verification: %s)\n",
                strerror(-result));
        return EXIT_FAILURE;
    }
    if (cleanup_result != 0) {
        fprintf(stderr, "ACQUIRE-ONLY RESULT: FAIL (cleanup: %s)\n",
                strerror(-cleanup_result));
        return EXIT_FAILURE;
    }
    printf("ACQUIRE-ONLY RESULT: PASS\n");
    return EXIT_SUCCESS;
}

static int run_scanout_test(unsigned int hold_seconds)
{
    struct scanout_state state;
    int result;
    int cleanup_result;

    printf("event=PROBE_STARTED monotonic_ns=%" PRIu64 " mode=scanout-test\n",
           monotonic_ns());
    result = prepare_active_topology(&state);
    if (result != 0) {
        fprintf(stderr, "SCANOUT RESULT: FAIL (active topology: %s)\n", strerror(-result));
        if (state.topo.fd >= 0)
            close_topology(&state.topo);
        return EXIT_FAILURE;
    }
    printf("selected_runtime     : card=%s connector=%u crtc=%u plane=%u mode=%s %.3fHz\n",
           state.topo.card, state.topo.connector_id, state.topo.crtc_id,
           state.topo.plane_id, state.topo.mode.name, mode_refresh(&state.topo.mode));

    result = acquire_master(&state);
    if (result == 0)
        result = create_dumb_buffer(&state);
    if (result == 0)
        result = atomic_scanout(&state);
    if (result == 0)
        finite_hold(hold_seconds);
    cleanup_result = cleanup_scanout(&state);

    if (result != 0) {
        fprintf(stderr, "SCANOUT RESULT: FAIL (%s)\n", strerror(-result));
        return EXIT_FAILURE;
    }
    if (cleanup_result != 0) {
        fprintf(stderr, "SCANOUT RESULT: FAIL (cleanup: %s)\n",
                strerror(-cleanup_result));
        return EXIT_FAILURE;
    }
    printf("SCANOUT RESULT: PASS (real atomic commit and finite clean release)\n");
    return EXIT_SUCCESS;
}

static void usage(FILE *stream, const char *program)
{
    fprintf(stream,
            "usage: %s MODE [--hold-seconds N]\n"
            "\n"
            "MODE (exactly one):\n"
            "  --inspect       passive topology inspection; never requests DRM master\n"
            "  --acquire-only  acquire, verify, hold briefly, and drop DRM master\n"
            "  --scanout-test  finite XRGB8888 dumb-buffer atomic KMS scanout\n"
            "\n"
            "Options:\n"
            "  --hold-seconds N  hold duration 1-%d seconds (default %d)\n"
            "  --help            show this help\n",
            program, MAX_HOLD_SECONDS, DEFAULT_HOLD_SECONDS);
}

int main(int argc, char **argv)
{
    enum run_mode mode = MODE_NONE;
    unsigned int hold_seconds = DEFAULT_HOLD_SECONDS;
    int i;

    for (i = 1; i < argc; ++i) {
        if (strcmp(argv[i], "--inspect") == 0) {
            if (mode != MODE_NONE) {
                fprintf(stderr, "exactly one mode is required\n");
                return 2;
            }
            mode = MODE_INSPECT;
        } else if (strcmp(argv[i], "--acquire-only") == 0) {
            if (mode != MODE_NONE) {
                fprintf(stderr, "exactly one mode is required\n");
                return 2;
            }
            mode = MODE_ACQUIRE_ONLY;
        } else if (strcmp(argv[i], "--scanout-test") == 0) {
            if (mode != MODE_NONE) {
                fprintf(stderr, "exactly one mode is required\n");
                return 2;
            }
            mode = MODE_SCANOUT_TEST;
        }
        else if (strcmp(argv[i], "--hold-seconds") == 0 && i + 1 < argc) {
            char *end = NULL;
            unsigned long value = strtoul(argv[++i], &end, 10);

            if (!end || *end != '\0' || value < 1 || value > MAX_HOLD_SECONDS) {
                fprintf(stderr, "invalid --hold-seconds value\n");
                return 2;
            }
            hold_seconds = (unsigned int)value;
        } else if (strcmp(argv[i], "--help") == 0) {
            usage(stdout, argv[0]);
            return EXIT_SUCCESS;
        } else {
            usage(stderr, argv[0]);
            return 2;
        }
    }

    if (mode == MODE_NONE) {
        usage(stderr, argv[0]);
        return 2;
    }
    if (mode == MODE_INSPECT && hold_seconds != DEFAULT_HOLD_SECONDS) {
        fprintf(stderr, "--hold-seconds is not valid with --inspect\n");
        return 2;
    }

    install_signal_handlers();
    switch (mode) {
    case MODE_INSPECT:
        return run_inspect();
    case MODE_ACQUIRE_ONLY:
        return run_acquire_only(hold_seconds);
    case MODE_SCANOUT_TEST:
        return run_scanout_test(hold_seconds);
    default:
        return 2;
    }
}
