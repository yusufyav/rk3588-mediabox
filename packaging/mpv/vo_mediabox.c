/*
 * vo_mediabox: draw on MediaBox's own video plane.
 *
 * This appliance's interface is not a compositor and never will be. It is one
 * process that holds DRM master on the Rockchip display controller and scans
 * its own Slint surface out on Cluster0, the primary window of video port 0.
 * The same video port drives a second window, Esmart0, which takes NV12 and
 * NV15 and has a scaler of its own:
 *
 *     Video Port0  PLANE_MASK  Cluster0 | Esmart0
 *     plane 57  Cluster0      Primary  zpos 0
 *     plane 73  Esmart0-win0           zpos 11  FEATURE scale=0x1
 *               NV12 NV21 NV16 NV61 NV24 NV42 NV15 NV20 NV30 ...
 *
 * So a film does not need a compositor and does not need the display: it needs
 * somebody who already holds master to put the decoder's own buffer on that
 * second window. This output does exactly that and nothing else. It decodes
 * through MPP as before, and instead of drawing anywhere it hands the frame's
 * dma-buf file descriptors to the interface over a unix socket, with SCM_RIGHTS.
 * The interface imports them on the card it has open, gives them a framebuffer
 * and calls SetPlane. Nothing is copied, nothing is converted on the CPU, and
 * the television never changes hands.
 *
 * The buffer goes back when it is safe to draw into again. SetPlane takes
 * effect at the next vertical blank, so the interface releases a frame only
 * once a later one has replaced it on the wire; until that release arrives this
 * output keeps its own reference to the image, which is what stops MPP handing
 * the same buffer back to the decoder while the panel is still reading it.
 *
 * The wire is one fixed-size message per frame so that a frame and its
 * descriptors can never be split across a read. It is written down in two
 * places: here, and in the interface's video.rs, whose test asserts the size.
 *
 *      0  magic 'MBV1'   16  fourcc      32  pitches[4]   64  modifier
 *      4  kind            20  width       48  offsets[4]   72  descriptors
 *      8  id              24  height                       76  flags
 *                         28  planes                       80  display width
 *                                                          84  display height
 *
 * This file is part of mpv, and is licensed under the same terms.
 */

#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#include <drm_fourcc.h>
#include <libavutil/hwcontext_drm.h>

#include "common/common.h"
#include "common/msg.h"
#include "video/img_format.h"
#include "video/mp_image.h"
#include "vo.h"

#define MBV_MAGIC     0x3156424dU  /* "MBV1" on a little-endian machine. */
#define MBV_FRAME     0U
#define MBV_STOP      1U
#define MBV_RELEASE   2U
#define MBV_MESSAGE   88
#define MBV_REPLY     16
#define MBV_MAX_FDS   4

/* How many frames may be with the interface before this output stops waiting.
 * Two are normally outstanding — the one on the panel and the one behind it.
 * More than this means the interface has stopped answering, and a player that
 * holds every buffer it ever decoded would starve MPP of its pool instead. */
#define MBV_IN_FLIGHT 8

#define MBV_SOCKET "/run/mediabox-ui/video.sock"

struct held {
    uint64_t id;
    struct mp_image *image;
};

struct priv {
    int fd;
    uint64_t next_id;
    struct held held[MBV_IN_FLIGHT];
    int nheld;
    int d_w, d_h;
    uint32_t colour;
    bool said_full;
};

/* Which matrix, which range and which transfer curve the frame carries.
 *
 * The display controller does the YCbCr to RGB conversion in hardware and
 * defaults to BT.601 limited, which is right for a DVD and wrong for everything
 * this appliance is for. Packed into one word: the encoding in the low four
 * bits in the plane's own enum order, the range in the next four, and the
 * transfer curve in the four above that, in the vendor driver's own EOTF
 * numbering so the interface passes it straight through.
 *
 * The transfer curve is the one that was missing, and its absence was not a
 * dull picture: the plane's EOTF is a property of the *plane*, not of the
 * frame, so it survives whoever set it last. Kodi leaves it on ST 2084 after
 * an HDR film, this player never wrote it, and the next SDR film was scanned
 * out as if it were PQ -- measured, `format: NV12  color: HDR10[2]`, and on
 * the television a magenta picture. */
static uint32_t colour_flags(const struct mp_image_params *params)
{
    uint32_t encoding;
    switch (params->repr.sys) {
    case PL_COLOR_SYSTEM_BT_709:
        encoding = 1;
        break;
    case PL_COLOR_SYSTEM_BT_2020_NC:
    case PL_COLOR_SYSTEM_BT_2020_C:
        encoding = 2;
        break;
    default:
        encoding = 0;
        break;
    }
    uint32_t full = params->repr.levels == PL_COLOR_LEVELS_FULL ? 1 : 0;

    /* The vendor driver's EOTF enum: 0 traditional gamma (SDR), 2 SMPTE
     * ST 2084, 3 BT.2100 HLG. Anything else is SDR as far as this plane is
     * concerned. */
    uint32_t eotf;
    switch (params->color.transfer) {
    case PL_COLOR_TRC_PQ:
        eotf = 2;
        break;
    case PL_COLOR_TRC_HLG:
        eotf = 3;
        break;
    default:
        eotf = 0;
        break;
    }
    return encoding | (full << 4) | (eotf << 8);
}

static void put32(uint8_t *at, uint32_t value)
{
    memcpy(at, &value, sizeof(value));
}

static void put64(uint8_t *at, uint64_t value)
{
    memcpy(at, &value, sizeof(value));
}

/* Let go of one frame, whatever the reason. */
static void release_held(struct priv *p, int index)
{
    talloc_free(p->held[index].image);
    for (int i = index; i + 1 < p->nheld; i++)
        p->held[i] = p->held[i + 1];
    p->nheld--;
}

/* Everything the interface has finished with, without waiting for it. */
static void drain_releases(struct vo *vo)
{
    struct priv *p = vo->priv;
    for (;;) {
        uint8_t reply[MBV_REPLY];
        ssize_t read = recv(p->fd, reply, sizeof(reply), MSG_DONTWAIT);
        if (read < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR)
                return;
            MP_WARN(vo, "MediaBox arayüzü ile bağlantı koptu: %s\n", strerror(errno));
            return;
        }
        if (read == 0) {
            MP_WARN(vo, "MediaBox arayüzü bağlantıyı kapattı\n");
            return;
        }
        if (read != MBV_REPLY)
            continue;
        uint32_t magic, kind;
        uint64_t id;
        memcpy(&magic, reply + 0, sizeof(magic));
        memcpy(&kind, reply + 4, sizeof(kind));
        memcpy(&id, reply + 8, sizeof(id));
        if (magic != MBV_MAGIC || kind != MBV_RELEASE)
            continue;
        for (int i = 0; i < p->nheld; i++) {
            if (p->held[i].id == id) {
                release_held(p, i);
                break;
            }
        }
    }
}

static bool send_message(struct vo *vo, const uint8_t *message,
                         const int *fds, int nfds)
{
    struct priv *p = vo->priv;

    struct iovec iov = {
        .iov_base = (void *)message,
        .iov_len = MBV_MESSAGE,
    };
    union {
        struct cmsghdr align;
        char bytes[CMSG_SPACE(MBV_MAX_FDS * sizeof(int))];
    } control;
    memset(&control, 0, sizeof(control));

    struct msghdr header = {
        .msg_iov = &iov,
        .msg_iovlen = 1,
    };
    if (nfds > 0) {
        header.msg_control = control.bytes;
        header.msg_controllen = CMSG_SPACE(nfds * sizeof(int));
        struct cmsghdr *cmsg = CMSG_FIRSTHDR(&header);
        cmsg->cmsg_level = SOL_SOCKET;
        cmsg->cmsg_type = SCM_RIGHTS;
        cmsg->cmsg_len = CMSG_LEN(nfds * sizeof(int));
        memcpy(CMSG_DATA(cmsg), fds, nfds * sizeof(int));
    }

    while (sendmsg(p->fd, &header, MSG_NOSIGNAL) < 0) {
        if (errno == EINTR)
            continue;
        MP_ERR(vo, "kare gönderilemedi: %s\n", strerror(errno));
        return false;
    }
    return true;
}

static int preinit(struct vo *vo)
{
    struct priv *p = vo->priv;

    const char *path = getenv("MEDIABOX_VIDEO_SOCKET");
    if (!path || !path[0])
        path = MBV_SOCKET;

    p->fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (p->fd < 0) {
        MP_ERR(vo, "socket: %s\n", strerror(errno));
        return -1;
    }

    struct sockaddr_un address = { .sun_family = AF_UNIX };
    if (snprintf(address.sun_path, sizeof(address.sun_path), "%s", path)
        >= (int)sizeof(address.sun_path)) {
        MP_ERR(vo, "soket yolu çok uzun: %s\n", path);
        close(p->fd);
        p->fd = -1;
        return -1;
    }
    if (connect(p->fd, (struct sockaddr *)&address, sizeof(address)) < 0) {
        /* The interface is what owns the panel. Without it there is nowhere to
         * draw, and saying which socket was tried is the difference between a
         * five minute diagnosis and an afternoon. */
        MP_ERR(vo, "MediaBox arayüzüne bağlanılamadı (%s): %s\n",
               path, strerror(errno));
        close(p->fd);
        p->fd = -1;
        return -1;
    }

    p->next_id = 1;
    MP_INFO(vo, "MediaBox video düzlemine çiziliyor (%s)\n", path);
    return 0;
}

static int query_format(struct vo *vo, int format)
{
    /* Only what the decoder produces without a copy. A software-decoded frame
     * has no dma-buf to hand over, and converting one here would be exactly the
     * copy this output exists to avoid: mpv falls back to another output
     * instead, and says so. */
    return format == IMGFMT_DRMPRIME;
}

static int reconfig(struct vo *vo, struct mp_image *img)
{
    struct priv *p = vo->priv;

    if (img->imgfmt != IMGFMT_DRMPRIME) {
        MP_ERR(vo, "donanım kod çözücüsünden gelmeyen bir kare: %s\n",
               mp_imgfmt_to_name(img->imgfmt));
        return VO_ERROR;
    }

    /* What the film should look like, not how it is stored. An anamorphic
     * transfer is 1920x1080 in the file and 2.39:1 on the television, and the
     * interface is the one that letterboxes it, so it has to be told. */
    mp_image_params_get_dsize(&img->params, &p->d_w, &p->d_h);
    p->colour = colour_flags(&img->params);
    MP_VERBOSE(vo, "kare %dx%d, gösterim %dx%d, renk %u\n",
               img->params.w, img->params.h, p->d_w, p->d_h, p->colour);
    return 0;
}

static bool draw_frame(struct vo *vo, struct vo_frame *frame)
{
    struct priv *p = vo->priv;

    drain_releases(vo);

    if (!frame->current)
        return VO_TRUE;

    struct mp_image *mpi = frame->current;
    AVDRMFrameDescriptor *desc = (AVDRMFrameDescriptor *)mpi->planes[0];
    if (!desc || desc->nb_layers < 1 || desc->nb_objects < 1) {
        MP_ERR(vo, "karede dma-buf tanımı yok\n");
        return VO_FALSE;
    }

    /* One layer: a whole picture. A decoder that split the luma and the chroma
     * into separate layers would need a framebuffer each and this display
     * controller shows one buffer per window, so it is refused here rather than
     * by an ioctl with a worse message. */
    if (desc->nb_layers > 1)
        MP_WARN(vo, "karede %d katman var, ilki kullanılıyor\n", desc->nb_layers);

    AVDRMLayerDescriptor *layer = &desc->layers[0];
    int nplanes = MPMIN(layer->nb_planes, MBV_MAX_FDS);

    int fds[MBV_MAX_FDS];
    uint32_t pitches[4] = {0}, offsets[4] = {0};
    for (int i = 0; i < nplanes; i++) {
        int object = layer->planes[i].object_index;
        fds[i] = desc->objects[object].fd;
        pitches[i] = layer->planes[i].pitch;
        offsets[i] = layer->planes[i].offset;
    }
    uint64_t modifier = desc->objects[layer->planes[0].object_index].format_modifier;
    if (modifier == DRM_FORMAT_MOD_INVALID)
        modifier = 0; /* LINEAR: what MPP produces on this board. */

    /* The interface has stopped answering. Letting go of the oldest frame is
     * the lesser fault: the buffer may still be on the panel for a moment, but
     * holding every one of them empties MPP's pool and the film stops. */
    while (p->nheld >= MBV_IN_FLIGHT) {
        if (!p->said_full) {
            MP_WARN(vo, "arayüz kareleri geri vermiyor\n");
            p->said_full = true;
        }
        release_held(p, 0);
    }

    uint64_t id = p->next_id++;
    uint8_t message[MBV_MESSAGE];
    memset(message, 0, sizeof(message));
    put32(message + 0, MBV_MAGIC);
    put32(message + 4, MBV_FRAME);
    put64(message + 8, id);
    put32(message + 16, layer->format);
    put32(message + 20, mpi->params.w);
    put32(message + 24, mpi->params.h);
    put32(message + 28, nplanes);
    for (int i = 0; i < 4; i++) {
        put32(message + 32 + i * 4, pitches[i]);
        put32(message + 48 + i * 4, offsets[i]);
    }
    put64(message + 64, modifier);
    put32(message + 72, nplanes);
    put32(message + 76, p->colour);
    put32(message + 80, p->d_w > 0 ? (uint32_t)p->d_w : 0);
    put32(message + 84, p->d_h > 0 ? (uint32_t)p->d_h : 0);

    if (!send_message(vo, message, fds, nplanes))
        return VO_FALSE;

    /* Our own reference, held until the interface says the panel is finished
     * with it. This is the whole of the buffer protocol on this side. */
    p->held[p->nheld++] = (struct held){
        .id = id,
        .image = mp_image_new_ref(mpi),
    };
    p->said_full = false;
    return VO_TRUE;
}

static void flip_page(struct vo *vo)
{
    /* Nothing. The frame was on the panel the moment the interface called
     * SetPlane; there is no second buffer here to swap. */
}

static int control(struct vo *vo, uint32_t request, void *data)
{
    return VO_NOTIMPL;
}

static void uninit(struct vo *vo)
{
    struct priv *p = vo->priv;
    if (p->fd < 0)
        return;

    /* Take the film off the panel before the process goes, so the interface
     * underneath is what the television shows again. Saying so is better than
     * letting the socket close: the interface does the same thing either way,
     * but only one of the two is a decision. */
    uint8_t message[MBV_MESSAGE];
    memset(message, 0, sizeof(message));
    put32(message + 0, MBV_MAGIC);
    put32(message + 4, MBV_STOP);
    send_message(vo, message, NULL, 0);

    while (p->nheld > 0)
        release_held(p, 0);

    close(p->fd);
    p->fd = -1;
}

const struct vo_driver video_out_mediabox = {
    .description = "MediaBox video plane (Rockchip VOP2, zero copy)",
    .name = "mediabox",
    .preinit = preinit,
    .query_format = query_format,
    .reconfig2 = reconfig,
    .control = control,
    .draw_frame = draw_frame,
    .flip_page = flip_page,
    .uninit = uninit,
    .priv_size = sizeof(struct priv),
};
