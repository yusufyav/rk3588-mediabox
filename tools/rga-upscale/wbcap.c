// wbcap <in.nv12> <W> <H> <out.nv12> <plane_id>
//
// Shows a tightly packed NV12 frame (BT.709, limited) on <plane_id>, scaled by
// the display controller to the CRTC's current mode, with every other plane on
// that CRTC off, and reads the composition back through the writeback
// connector as tightly packed NV12 at the mode's size. A 1920x1080 input is
// VOP2's own upscale; a 3840x2160 input on a 2160p mode is the 1:1 scan-out.
// The same path for both is what makes a VOP2-vs-RGA comparison fair.
//
// Diagnostic only, never part of the product. It needs DRM master, so stop
// mediabox-tv-ui first and start it again afterwards; plane 73 is Esmart0 on
// the Plus (see /sys/kernel/debug/dri/0/state). Writeback leaves the panel
// itself untrustworthy until the connector is unbound: check that VP0 in
// /sys/kernel/debug/dri/0/summary names HDMI-A-1 alone before judging the
// television again.
//
//   gcc -O2 -o wbcap wbcap.c $(pkg-config --cflags --libs libdrm)
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <drm_fourcc.h>
#include <xf86drm.h>
#include <xf86drmMode.h>

#define die(...) do { fprintf(stderr, __VA_ARGS__); fputc('\n', stderr); exit(1); } while (0)

struct fb { uint32_t id, pitch, w, h; uint64_t size; uint8_t *map; };

static void fb_nv12(int fd, uint32_t w, uint32_t h, struct fb *f)
{
	struct drm_mode_create_dumb c = { .width = w, .height = h * 3 / 2, .bpp = 8 };
	if (drmIoctl(fd, DRM_IOCTL_MODE_CREATE_DUMB, &c)) die("dumb: %s", strerror(errno));
	uint32_t hd[4] = { c.handle, c.handle }, pi[4] = { c.pitch, c.pitch }, of[4] = { 0, c.pitch * h };
	if (drmModeAddFB2(fd, w, h, DRM_FORMAT_NV12, hd, pi, of, &f->id, 0)) die("addfb: %s", strerror(errno));
	struct drm_mode_map_dumb m = { .handle = c.handle };
	if (drmIoctl(fd, DRM_IOCTL_MODE_MAP_DUMB, &m)) die("map");
	f->map = mmap(0, c.size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, m.offset);
	if (f->map == MAP_FAILED) die("mmap");
	f->pitch = c.pitch; f->w = w; f->h = h; f->size = c.size;
}

static uint32_t prop(int fd, uint32_t obj, uint32_t type, const char *name, uint64_t *enum_val, const char *enum_name)
{
	drmModeObjectProperties *p = drmModeObjectGetProperties(fd, obj, type);
	uint32_t id = 0;
	for (uint32_t i = 0; p && i < p->count_props; i++) {
		drmModePropertyRes *r = drmModeGetProperty(fd, p->props[i]);
		if (r && !strcmp(r->name, name)) {
			id = r->prop_id;
			for (int e = 0; enum_name && e < r->count_enums; e++)
				if (!strcmp(r->enums[e].name, enum_name)) *enum_val = r->enums[e].value;
		}
		drmModeFreeProperty(r);
	}
	drmModeFreeObjectProperties(p);
	return id;
}

static void add(drmModeAtomicReq *q, int fd, uint32_t obj, uint32_t type, const char *name, uint64_t v)
{
	uint32_t id = prop(fd, obj, type, name, NULL, NULL);
	if (id) drmModeAtomicAddProperty(q, obj, id, v);
	else fprintf(stderr, "(no %s on %u)\n", name, obj);
}

int main(int argc, char **argv)
{
	if (argc < 6) die("usage: wbcap in.nv12 W H out.nv12 plane_id");
	uint32_t W = atoi(argv[2]), H = atoi(argv[3]), vplane = atoi(argv[5]);
	int fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
	drmSetClientCap(fd, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1);
	if (drmSetClientCap(fd, DRM_CLIENT_CAP_ATOMIC, 1)) die("atomic");
	if (drmSetClientCap(fd, DRM_CLIENT_CAP_WRITEBACK_CONNECTORS, 1)) die("wb cap");
	if (!drmIsMaster(fd) && drmSetMaster(fd)) die("not master: stop the UI");
	drmModeRes *res = drmModeGetResources(fd);
	drmModeConnector *disp = NULL, *wb = NULL;
	for (int i = 0; i < res->count_connectors; i++) {
		drmModeConnector *c = drmModeGetConnector(fd, res->connectors[i]);
		if (c->connector_type == DRM_MODE_CONNECTOR_WRITEBACK && !wb) wb = c;
		else if (c->connection == DRM_MODE_CONNECTED && c->encoder_id && !disp) disp = c;
	}
	if (!disp || !wb) die("no display/writeback");
	drmModeEncoder *enc = drmModeGetEncoder(fd, disp->encoder_id);
	drmModeCrtc *crtc = drmModeGetCrtc(fd, enc->crtc_id);
	if (!crtc || !crtc->mode_valid) die("crtc has no mode");
	drmModeModeInfo mode = crtc->mode;
	printf("crtc %u mode %ux%u@%u\n", crtc->crtc_id, mode.hdisplay, mode.vdisplay, mode.vrefresh);

	struct fb v, w;
	fb_nv12(fd, W, H, &v);
	FILE *in = fopen(argv[1], "rb");
	if (!in) die("open %s", argv[1]);
	for (uint32_t y = 0; y < H * 3 / 2; y++)
		if (fread(v.map + (uint64_t)y * v.pitch, 1, W, in) != W) die("short read");
	fclose(in);
	fb_nv12(fd, mode.hdisplay, mode.vdisplay, &w);

	uint64_t enc709 = 0, lim = 0;
	uint32_t pe = prop(fd, vplane, DRM_MODE_OBJECT_PLANE, "COLOR_ENCODING", &enc709, "ITU-R BT.709 YCbCr");
	uint32_t pr = prop(fd, vplane, DRM_MODE_OBJECT_PLANE, "COLOR_RANGE", &lim, "YCbCr limited range");

	drmModePlaneRes *pres = drmModeGetPlaneResources(fd);
	for (int pass = 0; pass < 2; pass++) {
		drmModeAtomicReq *q = drmModeAtomicAlloc();
		for (uint32_t i = 0; i < pres->count_planes; i++) {
			drmModePlane *pl = drmModeGetPlane(fd, pres->planes[i]);
			if (pl->plane_id != vplane && pl->crtc_id == crtc->crtc_id) {
				add(q, fd, pl->plane_id, DRM_MODE_OBJECT_PLANE, "FB_ID", 0);
				add(q, fd, pl->plane_id, DRM_MODE_OBJECT_PLANE, "CRTC_ID", 0);
			}
			drmModeFreePlane(pl);
		}
		uint32_t P = DRM_MODE_OBJECT_PLANE;
		add(q, fd, vplane, P, "FB_ID", v.id);
		add(q, fd, vplane, P, "CRTC_ID", crtc->crtc_id);
		add(q, fd, vplane, P, "SRC_X", 0); add(q, fd, vplane, P, "SRC_Y", 0);
		add(q, fd, vplane, P, "SRC_W", (uint64_t)W << 16); add(q, fd, vplane, P, "SRC_H", (uint64_t)H << 16);
		add(q, fd, vplane, P, "CRTC_X", 0); add(q, fd, vplane, P, "CRTC_Y", 0);
		add(q, fd, vplane, P, "CRTC_W", mode.hdisplay); add(q, fd, vplane, P, "CRTC_H", mode.vdisplay);
		if (pe) drmModeAtomicAddProperty(q, vplane, pe, enc709);
		if (pr) drmModeAtomicAddProperty(q, vplane, pr, lim);
		add(q, fd, vplane, P, "EOTF", 0);
		int32_t fence = -1;
		if (pass == 1) {
			add(q, fd, wb->connector_id, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID", crtc->crtc_id);
			add(q, fd, wb->connector_id, DRM_MODE_OBJECT_CONNECTOR, "WRITEBACK_FB_ID", w.id);
			add(q, fd, wb->connector_id, DRM_MODE_OBJECT_CONNECTOR, "WRITEBACK_OUT_FENCE_PTR", (uint64_t)(uintptr_t)&fence);
		}
		if (drmModeAtomicCommit(fd, q, DRM_MODE_ATOMIC_ALLOW_MODESET, NULL)) die("commit %d: %s", pass, strerror(errno));
		drmModeAtomicFree(q);
		if (pass == 0) { usleep(300000); continue; }
		struct pollfd p = { .fd = fence, .events = POLLIN };
		if (fence < 0 || poll(&p, 1, 2000) <= 0) die("writeback fence failed");
	}
	FILE *out = fopen(argv[4], "wb");
	for (uint32_t y = 0; y < mode.vdisplay * 3 / 2; y++)
		fwrite(w.map + (uint64_t)y * w.pitch, 1, mode.hdisplay, out);
	fclose(out);
	printf("wrote %s %ux%u nv12 (pitch %u)\n", argv[4], mode.hdisplay, mode.vdisplay, w.pitch);
	return 0;
}
