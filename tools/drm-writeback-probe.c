/*
 * drm-writeback-probe -- capture the composited CRTC output of the RK3588 VOP2
 * through the DRM writeback connector.
 *
 * Why this tool exists: the Kodi pause/resume horizontal shift is invisible in
 * the VOP2 register file -- every geometry register is bit-identical whether
 * the GUI/OSD plane is in the mix or not -- yet the picture provably moves, on
 * more than one sink. Register state is therefore not a sufficient witness, and
 * the argument has to be settled on pixels. DRM writeback taps the CRTC's
 * composited output into a memory framebuffer, which is the last point inside
 * the SoC where the frame can be read back, so it separates "the VOP2 emitted a
 * shifted frame" from "everything downstream of the VOP2 shifted it".
 *
 * `audit` is read-only and safe to run while another process is DRM master: it
 * only enumerates. Capturing needs the CRTC, hence DRM master, hence Kodi has
 * to be stopped first -- the tool refuses rather than fighting over the display.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <inttypes.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <poll.h>
#include <unistd.h>
#include <sys/mman.h>

#include <drm_fourcc.h>
#include <drm_mode.h>
#include <xf86drm.h>
#include <xf86drmMode.h>

static void die(const char *fmt, ...)
{
	va_list ap;
	va_start(ap, fmt);
	vfprintf(stderr, fmt, ap);
	va_end(ap);
	fputc('\n', stderr);
	exit(1);
}

static const char *conn_type_name(uint32_t type)
{
	switch (type) {
	case DRM_MODE_CONNECTOR_HDMIA:     return "HDMI-A";
	case DRM_MODE_CONNECTOR_WRITEBACK: return "Writeback";
	case DRM_MODE_CONNECTOR_eDP:       return "eDP";
	case DRM_MODE_CONNECTOR_DisplayPort: return "DP";
	default:                           return "other";
	}
}

/* Prints one object's properties, resolving enums and blob sizes, because the
 * writeback contract (its formats, its CRTC set, its fence property) is exactly
 * what has to be recorded before any capture is attempted. */
static void dump_props(int fd, uint32_t obj_id, uint32_t obj_type, const char *label)
{
	drmModeObjectProperties *props =
		drmModeObjectGetProperties(fd, obj_id, obj_type);
	if (!props) {
		printf("  %s: no properties (%s)\n", label, strerror(errno));
		return;
	}
	for (uint32_t i = 0; i < props->count_props; i++) {
		drmModePropertyRes *p = drmModeGetProperty(fd, props->props[i]);
		if (!p)
			continue;
		printf("    %-28s = %" PRIu64, p->name, (uint64_t)props->prop_values[i]);
		if (p->flags & DRM_MODE_PROP_ENUM) {
			for (int e = 0; e < p->count_enums; e++)
				if (p->enums[e].value == props->prop_values[i])
					printf(" (%s)", p->enums[e].name);
		}
		if (p->flags & DRM_MODE_PROP_BLOB && props->prop_values[i]) {
			drmModePropertyBlobRes *b =
				drmModeGetPropertyBlob(fd, props->prop_values[i]);
			if (b) {
				printf(" [blob %u bytes]", b->length);
				/* The writeback format list is the one blob whose
				 * contents decide whether a capture is possible. */
				if (!strcmp(p->name, "WRITEBACK_PIXEL_FORMATS")) {
					uint32_t *fmts = b->data;
					printf("\n      formats:");
					for (uint32_t f = 0; f < b->length / 4; f++)
						printf(" %.4s", (char *)&fmts[f]);
				}
				drmModeFreePropertyBlob(b);
			}
		}
		printf("\n");
		drmModeFreeProperty(p);
	}
	drmModeFreeObjectProperties(props);
}

static int cmd_audit(const char *path)
{
	int fd = open(path, O_RDWR | O_CLOEXEC);
	if (fd < 0)
		die("open %s: %s", path, strerror(errno));

	/* Both caps are required before the kernel will even show a writeback
	 * connector; without them the enumeration below silently omits it and
	 * the absence would look like "this SoC has no writeback". */
	if (drmSetClientCap(fd, DRM_CLIENT_CAP_ATOMIC, 1))
		printf("# WARNING: atomic cap refused: %s\n", strerror(errno));
	if (drmSetClientCap(fd, DRM_CLIENT_CAP_WRITEBACK_CONNECTORS, 1))
		printf("# WARNING: writeback cap refused: %s\n", strerror(errno));

	drmVersionPtr ver = drmGetVersion(fd);
	if (ver) {
		printf("driver: %s %d.%d.%d (%s)\n", ver->name, ver->version_major,
		       ver->version_minor, ver->version_patchlevel, ver->desc);
		drmFreeVersion(ver);
	}
	printf("master: %s\n", drmIsMaster(fd) ? "yes" : "no (another process holds it)");

	drmModeRes *res = drmModeGetResources(fd);
	if (!res)
		die("drmModeGetResources: %s", strerror(errno));

	printf("\ncrtcs:\n");
	for (int i = 0; i < res->count_crtcs; i++)
		printf("  [%d] id=%u\n", i, res->crtcs[i]);

	printf("\nconnectors:\n");
	for (int i = 0; i < res->count_connectors; i++) {
		drmModeConnector *c = drmModeGetConnector(fd, res->connectors[i]);
		if (!c)
			continue;
		printf("  id=%u type=%s-%u status=%s encoders=%d\n", c->connector_id,
		       conn_type_name(c->connector_type), c->connector_type_id,
		       c->connection == DRM_MODE_CONNECTED ? "connected" :
		       c->connection == DRM_MODE_DISCONNECTED ? "disconnected" : "unknown",
		       c->count_encoders);
		/* possible_crtcs for a connector is carried by its encoders, and
		 * it is what says whether writeback can observe VP0 at all. */
		for (int e = 0; e < c->count_encoders; e++) {
			drmModeEncoder *enc = drmModeGetEncoder(fd, c->encoders[e]);
			if (enc) {
				printf("    encoder id=%u possible_crtcs=0x%x\n",
				       enc->encoder_id, enc->possible_crtcs);
				drmModeFreeEncoder(enc);
			}
		}
		dump_props(fd, c->connector_id, DRM_MODE_OBJECT_CONNECTOR, "connector");
		drmModeFreeConnector(c);
	}

	/* Planes carry the geometry properties under test, and their format
	 * lists decide what a synthetic reproduction is allowed to program. */
	drmModePlaneRes *pres = drmModeGetPlaneResources(fd);
	if (pres) {
		printf("\nplanes:\n");
		for (uint32_t i = 0; i < pres->count_planes; i++) {
			drmModePlane *pl = drmModeGetPlane(fd, pres->planes[i]);
			if (!pl)
				continue;
			printf("  id=%u possible_crtcs=0x%x crtc=%u fb=%u formats=",
			       pl->plane_id, pl->possible_crtcs, pl->crtc_id, pl->fb_id);
			for (uint32_t f = 0; f < pl->count_formats; f++)
				printf(" %.4s", (char *)&pl->formats[f]);
			printf("\n");
			dump_props(fd, pl->plane_id, DRM_MODE_OBJECT_PLANE, "plane");
			drmModeFreePlane(pl);
		}
		drmModeFreePlaneResources(pres);
	}

	drmModeFreeResources(res);
	close(fd);
	return 0;
}


/* ---------------------------------------------------------------- capture */

struct fb {
	uint32_t fb_id, handle, pitch, size, w, h, fourcc;
	uint8_t *map;
};

static int fb_create(int fd, uint32_t w, uint32_t h, uint32_t fourcc, struct fb *out)
{
	/* NV12 is semi-planar: one dumb buffer tall enough for Y followed by
	 * interleaved CbCr at half height, exposed as two framebuffer planes
	 * that share the handle. It is here because this SoC's writeback
	 * refuses RGB targets -- the CRTC output is YCbCr and the hardware has
	 * no YUV-to-RGB stage on the writeback path. */
	int nv12 = fourcc == DRM_FORMAT_NV12;
	struct drm_mode_create_dumb creq = {
		.width = w,
		.height = nv12 ? h * 3 / 2 : h,
		.bpp = nv12 ? 8 : 32,
	};
	if (drmIoctl(fd, DRM_IOCTL_MODE_CREATE_DUMB, &creq))
		return -1;
	uint32_t handles[4] = { creq.handle }, pitches[4] = { creq.pitch }, offsets[4] = { 0 };
	if (nv12) {
		handles[1] = creq.handle;
		pitches[1] = creq.pitch;
		offsets[1] = creq.pitch * h;
	}
	uint32_t fb_id;
	if (drmModeAddFB2(fd, w, h, fourcc, handles, pitches, offsets, &fb_id, 0))
		return -1;
	struct drm_mode_map_dumb mreq = { .handle = creq.handle };
	if (drmIoctl(fd, DRM_IOCTL_MODE_MAP_DUMB, &mreq))
		return -1;
	void *map = mmap(0, creq.size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, mreq.offset);
	if (map == MAP_FAILED)
		return -1;
	*out = (struct fb){ fb_id, creq.handle, creq.pitch, creq.size, w, h, fourcc, map };
	return 0;
}

/*
 * The pattern has to make a horizontal displacement measurable without
 * subpixel work, so it is built from hard vertical edges: a ruler of 1px
 * columns every 16px, heavier marks every 256px, and solid blocks hugging both
 * the left and the right edge. If the frame is translated, the left block is
 * clipped and the right block loses columns -- which is precisely the reported
 * symptom, expressed in pixels a diff can count.
 */
static void fb_pattern(struct fb *f, uint32_t tint)
{
	for (uint32_t y = 0; y < f->h; y++) {
		uint32_t *row = (uint32_t *)(f->map + (size_t)y * f->pitch);
		for (uint32_t x = 0; x < f->w; x++) {
			uint32_t v = 0xff000000u | tint;
			if (x < 8 || x >= f->w - 8)
				v = 0xffffffffu;              /* edge blocks */
			else if (x % 256 == 0)
				v = 0xff00ff00u;              /* coarse ruler */
			else if (x % 16 == 0)
				v = 0xffffffffu;              /* fine ruler */
			else if (y % 128 == 0)
				v = 0xff404040u;              /* horizontal guides */
			row[x] = v;
		}
	}
}

/*
 * The same ruler, written as 8-bit semi-planar YUV. Format matters to this
 * experiment: an RGB plane and a YUV plane take different routes through the
 * VOP2's colour pipeline, and the fault under test only ever appears with a
 * YUV video layer in the mix.
 */
static void fb_pattern_nv12(struct fb *f)
{
	uint8_t *y = f->map;
	uint8_t *uv = f->map + (size_t)f->pitch * f->h;
	/*
	 * The column signature is deliberately APERIODIC. A regular ruler makes
	 * a displacement visible to the eye but ambiguous to cross-correlation:
	 * a shift of six against a period of sixteen aliases against every other
	 * candidate and the peak means nothing. A deterministic pseudo-random
	 * sequence gives exactly one sharp peak, so the measured dx is the
	 * displacement rather than one of many equally good answers.
	 */
	uint8_t sig[8192];
	/*
	 * Aperiodic, but deliberately LOW frequency: eight-pixel bars at
	 * irregular spacing on a flat field.
	 *
	 * Two constraints shape this. Cross-correlation needs the signature not
	 * to repeat, or a shift of six against a period of sixteen aliases and
	 * the peak is meaningless. And the signal has to survive the pipeline:
	 * a per-pixel random pattern sits at Nyquist, where the 10-bit YUV
	 * conversion and the writeback's 4:2:0 sampling alter the values
	 * themselves, so the two captures stop being shifted copies of each
	 * other and correlate against nothing.
	 */
	memset(sig, 40, sizeof(sig));
	uint32_t rng = 0x2f6e2b1u, x = 24;
	while (x + 8 < f->w && x + 8 < sizeof(sig)) {
		memset(sig + x, 200, 8);
		rng = rng * 1664525u + 1013904223u;
		x += 40 + ((rng >> 18) & 0x3f);   /* 40..103 px until the next bar */
	}
	for (uint32_t row = 0; row < f->h; row++) {
		uint8_t *p = y + (size_t)row * f->pitch;
		for (uint32_t col = 0; col < f->w; col++) {
			uint8_t v = sig[col];
			/* Solid edge blocks stay: they are what the operator can see
			 * on the panel, and they bound the displacement visually. */
			if (col < 8 || col >= f->w - 8)
				v = 235;
			p[col] = v;
		}
	}
	/* Neutral chroma: the measurement reads luma only, and flat chroma keeps
	 * the pattern grey so any colour shift would be visible rather than
	 * blended into the test signal. */
	memset(uv, 128, (size_t)f->pitch * (f->h / 2));
}

/* A GUI-like surface: fully transparent except for a marker, so enabling it
 * changes the composition without hiding the layer underneath. */
static void fb_overlay(struct fb *f)
{
	memset(f->map, 0, f->size);
	for (uint32_t y = 64; y < 192 && y < f->h; y++) {
		uint32_t *row = (uint32_t *)(f->map + (size_t)y * f->pitch);
		for (uint32_t x = 512; x < 1536 && x < f->w; x++)
			row[x] = 0xc00080ffu;
	}
}

struct props { drmModeObjectProperties *p; drmModePropertyRes **r; };

static int props_get(int fd, uint32_t id, uint32_t type, struct props *o)
{
	o->p = drmModeObjectGetProperties(fd, id, type);
	if (!o->p)
		return -1;
	o->r = calloc(o->p->count_props, sizeof(*o->r));
	for (uint32_t i = 0; i < o->p->count_props; i++)
		o->r[i] = drmModeGetProperty(fd, o->p->props[i]);
	return 0;
}

static uint32_t prop_id(struct props *o, const char *name)
{
	for (uint32_t i = 0; i < o->p->count_props; i++)
		if (o->r[i] && !strcmp(o->r[i]->name, name))
			return o->r[i]->prop_id;
	return 0;
}

/* Adding a property that the object does not have is a silent no-op here on
 * purpose: the same plane-setup code runs against Cluster and Esmart windows,
 * whose property sets differ, and a missing optional property is not an error. */
static void add(drmModeAtomicReq *req, struct props *o, uint32_t id,
		const char *name, uint64_t val)
{
	uint32_t pid = prop_id(o, name);
	if (pid)
		drmModeAtomicAddProperty(req, id, pid, val);
}

/*
 * One writeback frame into `path`. Waiting on the out fence is not optional:
 * reading the buffer while the VOP2 is still filling it would invent exactly
 * the kind of misalignment this tool exists to measure.
 */
struct layer_state {
	struct props *gp;
	uint32_t gui_plane, gui_fb, crtc_id, w, h;
	int on, hdr, gui_eotf;
};

/* The GUI layer's state is re-stated inside the writeback request rather than
 * left to whatever the previous commit installed. A writeback that carries no
 * plane state races the composition: the hardware may latch the capture while
 * the mixer is between configurations, and the result is a frame whose rows
 * disagree with each other -- torn output that reads convincingly as a shift
 * if only one row is inspected. */
static void add_layer(drmModeAtomicReq *req, struct layer_state *ls)
{
	if (!ls)
		return;
	if (ls->on) {
		add(req, ls->gp, ls->gui_plane, "FB_ID", ls->gui_fb);
		add(req, ls->gp, ls->gui_plane, "CRTC_ID", ls->crtc_id);
		add(req, ls->gp, ls->gui_plane, "SRC_X", 0);
		add(req, ls->gp, ls->gui_plane, "SRC_Y", 0);
		add(req, ls->gp, ls->gui_plane, "SRC_W", (uint64_t)ls->w << 16);
		add(req, ls->gp, ls->gui_plane, "SRC_H", (uint64_t)ls->h << 16);
		add(req, ls->gp, ls->gui_plane, "CRTC_X", 0);
		add(req, ls->gp, ls->gui_plane, "CRTC_Y", 0);
		add(req, ls->gp, ls->gui_plane, "CRTC_W", ls->w);
		add(req, ls->gp, ls->gui_plane, "CRTC_H", ls->h);
		if (ls->gui_eotf >= 0)
			add(req, ls->gp, ls->gui_plane, "EOTF", ls->gui_eotf);
		else if (ls->hdr)
			add(req, ls->gp, ls->gui_plane, "EOTF", 2);
	} else {
		add(req, ls->gp, ls->gui_plane, "FB_ID", 0);
		add(req, ls->gp, ls->gui_plane, "CRTC_ID", 0);
	}
}

static int writeback_once(int fd, drmModeConnector *wb, uint32_t crtc_id,
			  drmModeModeInfo *mode, const char *path,
			  struct layer_state *ls)
{
	struct fb wfb;
	if (fb_create(fd, mode->hdisplay, mode->vdisplay, DRM_FORMAT_NV12, &wfb)) {
		printf("writeback fb: %s\n", strerror(errno));
		return -1;
	}
	struct props wp;
	props_get(fd, wb->connector_id, DRM_MODE_OBJECT_CONNECTOR, &wp);
	int32_t out_fence = -1;
	drmModeAtomicReq *wreq = drmModeAtomicAlloc();
	add_layer(wreq, ls);
	add(wreq, &wp, wb->connector_id, "CRTC_ID", crtc_id);
	add(wreq, &wp, wb->connector_id, "WRITEBACK_FB_ID", wfb.fb_id);
	add(wreq, &wp, wb->connector_id, "WRITEBACK_OUT_FENCE_PTR",
	    (uint64_t)(uintptr_t)&out_fence);
	if (drmModeAtomicCommit(fd, wreq, DRM_MODE_ATOMIC_ALLOW_MODESET, NULL)) {
		printf("writeback commit: %s\n", strerror(errno));
		drmModeAtomicFree(wreq);
		return -1;
	}
	drmModeAtomicFree(wreq);

	if (out_fence >= 0) {
		struct pollfd pfd = { .fd = out_fence, .events = POLLIN };
		int pr = poll(&pfd, 1, 2000);
		if (pr <= 0)
			printf("writeback fence: %s\n", pr == 0 ? "TIMEOUT" : strerror(errno));
		close(out_fence);
	} else {
		printf("writeback fence: not produced\n");
	}

	FILE *f = fopen(path, "wb");
	if (!f) {
		printf("open %s: %s\n", path, strerror(errno));
		return -1;
	}
	fwrite(wfb.map, 1, wfb.size, f);
	fclose(f);
	printf("writeback: %s %ux%u fourcc=NV12 pitch=%u bytes=%u\n",
	       path, wfb.w, wfb.h, wfb.pitch, wfb.size);
	return 0;
}

struct opts {
	const char *card, *wb_path;
	int gui, hold, hdr, toggle, nv12, hdr_out;
	/* -1 = leave the GUI plane's EOTF alone; otherwise force this value.
	 * Tagging the GUI plane as PQ is what patch 0009 added, so whether the
	 * displacement depends on it is a question with a Kodi-level answer. */
	int gui_eotf;
	/* Which window acts as the GUI layer. The default picks the primary,
	 * which on this SoC is a Cluster window; forcing an Esmart window here
	 * asks whether the instability belongs to PQ compositing in general or
	 * to the Cluster pipeline specifically. */
	uint32_t gui_plane_override;
	uint32_t width, height, refresh;
	/* Kodi does not put the video plane on screen 1:1 -- it scales a
	 * 3840x2080 source into 3840x2076 at y=42. The scaler is therefore part
	 * of the configuration under test and has to be reproducible here. */
	uint32_t src_w, src_h, dst_w, dst_h, dst_x, dst_y;
};

/*
 * Reproduces the composition change under test and, optionally, reads the
 * result back through the writeback connector.
 *
 * The point of doing this outside Kodi is attribution: if the picture moves
 * when a second plane joins the mix here -- with a static pattern, no decoder,
 * no player logic -- then nothing above the DRM layer can be responsible.
 */
static int cmd_run(struct opts *o)
{
	int fd = open(o->card, O_RDWR | O_CLOEXEC);
	if (fd < 0)
		die("open %s: %s", o->card, strerror(errno));
	if (drmSetClientCap(fd, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1))
		die("universal planes: %s", strerror(errno));
	if (drmSetClientCap(fd, DRM_CLIENT_CAP_ATOMIC, 1))
		die("atomic: %s", strerror(errno));
	if (drmSetClientCap(fd, DRM_CLIENT_CAP_WRITEBACK_CONNECTORS, 1))
		printf("# writeback connectors unavailable: %s\n", strerror(errno));
	if (!drmIsMaster(fd) && drmSetMaster(fd))
		die("not DRM master (%s) -- stop the compositor first", strerror(errno));

	drmModeRes *res = drmModeGetResources(fd);
	if (!res)
		die("resources: %s", strerror(errno));

	/* Pick the connected display and the writeback tap in one pass, so the
	 * tool works on any RK3588 board without hard-coded object ids. */
	drmModeConnector *disp = NULL, *wb = NULL;
	for (int i = 0; i < res->count_connectors; i++) {
		drmModeConnector *c = drmModeGetConnector(fd, res->connectors[i]);
		if (!c)
			continue;
		if (c->connector_type == DRM_MODE_CONNECTOR_WRITEBACK && !wb)
			wb = c;
		else if (c->connection == DRM_MODE_CONNECTED && c->count_modes && !disp)
			disp = c;
		else
			drmModeFreeConnector(c);
	}
	if (!disp)
		die("no connected display");

	drmModeModeInfo *mode = NULL;
	for (int i = 0; i < disp->count_modes; i++) {
		drmModeModeInfo *m = &disp->modes[i];
		if (m->hdisplay == o->width && m->vdisplay == o->height &&
		    (!o->refresh || m->vrefresh == o->refresh)) {
			mode = m;
			break;
		}
	}
	if (!mode)
		mode = &disp->modes[0];
	printf("display: connector=%u mode=%ux%u@%u\n", disp->connector_id,
	       mode->hdisplay, mode->vdisplay, mode->vrefresh);

	drmModeEncoder *enc = drmModeGetEncoder(fd, disp->encoders[0]);
	uint32_t crtc_id = 0;
	for (int i = 0; i < res->count_crtcs; i++)
		if (enc->possible_crtcs & (1 << i)) {
			crtc_id = res->crtcs[i];
			break;
		}
	if (!crtc_id)
		die("no usable crtc");

	/* The video-like layer is the lowest-zpos plane that can take YUV, and
	 * the GUI-like layer is the primary; that is the same pairing Kodi ends
	 * up with on this SoC, arrived at by capability rather than by id. */
	drmModePlaneRes *pres = drmModeGetPlaneResources(fd);
	uint32_t vid_plane = 0, gui_plane = 0;
	for (uint32_t i = 0; i < pres->count_planes; i++) {
		drmModePlane *pl = drmModeGetPlane(fd, pres->planes[i]);
		if (!pl || !(pl->possible_crtcs & 1))
			continue;
		struct props pp;
		if (props_get(fd, pl->plane_id, DRM_MODE_OBJECT_PLANE, &pp) == 0) {
			uint64_t type = 0, zpos = 0;
			for (uint32_t k = 0; k < pp.p->count_props; k++) {
				if (pp.r[k] && !strcmp(pp.r[k]->name, "type"))
					type = pp.p->prop_values[k];
				if (pp.r[k] && !strcmp(pp.r[k]->name, "zpos"))
					zpos = pp.p->prop_values[k];
			}
			int yuv = 0;
			for (uint32_t f = 0; f < pl->count_formats; f++)
				if (pl->formats[f] == DRM_FORMAT_NV12)
					yuv = 1;
			if (type == DRM_PLANE_TYPE_PRIMARY && !gui_plane)
				gui_plane = pl->plane_id;
			else if (yuv && zpos == 0 && !vid_plane)
				vid_plane = pl->plane_id;
		}
		drmModeFreePlane(pl);
	}
	if (o->gui_plane_override)
		gui_plane = o->gui_plane_override;
	if (!vid_plane || !gui_plane)
		die("could not identify video (%u) / gui (%u) planes", vid_plane, gui_plane);
	printf("planes: video=%u gui=%u crtc=%u\n", vid_plane, gui_plane, crtc_id);

	struct fb vfb, gfb;
	uint32_t vfmt = o->nv12 ? DRM_FORMAT_NV12 : DRM_FORMAT_ARGB8888;
	if (fb_create(fd, mode->hdisplay, mode->vdisplay, vfmt, &vfb))
		die("video fb: %s", strerror(errno));
	if (o->nv12)
		fb_pattern_nv12(&vfb);
	else
		fb_pattern(&vfb, 0x00303030);
	if (fb_create(fd, mode->hdisplay, mode->vdisplay, DRM_FORMAT_ARGB8888, &gfb))
		die("gui fb: %s", strerror(errno));
	fb_overlay(&gfb);

	uint32_t mode_blob;
	if (drmModeCreatePropertyBlob(fd, mode, sizeof(*mode), &mode_blob))
		die("mode blob: %s", strerror(errno));

	struct props cp, rp, vp, gp;
	props_get(fd, disp->connector_id, DRM_MODE_OBJECT_CONNECTOR, &cp);
	props_get(fd, crtc_id, DRM_MODE_OBJECT_CRTC, &rp);
	props_get(fd, vid_plane, DRM_MODE_OBJECT_PLANE, &vp);
	props_get(fd, gui_plane, DRM_MODE_OBJECT_PLANE, &gp);

	/*
	 * Driving the output into HDR10 is not cosmetic here. Without it the VOP2
	 * runs this port as SDR RGB888 with overlay_mode 0, which is a different
	 * composition path from the YUV422 10-bit HDR10 pipeline (overlay_mode 1)
	 * the player actually uses -- so a negative result obtained in SDR would
	 * say nothing about the configuration under test.
	 */
	uint32_t hdr_blob = 0;
	if (o->hdr_out) {
		/* The kernel's own struct, not a hand-rolled equivalent: the blob is
		 * size-checked on commit, and a packed look-alike is two bytes short
		 * of the padded 32-byte layout and is rejected with a bare EINVAL. */
		struct hdr_output_metadata md;
		memset(&md, 0, sizeof(md));
		md.metadata_type = 0;
		md.hdmi_metadata_type1.eotf = 2;   /* SMPTE ST 2084 */
		md.hdmi_metadata_type1.metadata_type = 0;
		/* BT.2020 primaries and D65, in the 0.00002 units the infoframe uses */
		static const uint16_t prim[3][2] = { {8500, 39850}, {35400, 14600}, {6550, 2300} };
		for (int i = 0; i < 3; i++) {
			md.hdmi_metadata_type1.display_primaries[i].x = prim[i][0];
			md.hdmi_metadata_type1.display_primaries[i].y = prim[i][1];
		}
		md.hdmi_metadata_type1.white_point.x = 15635;
		md.hdmi_metadata_type1.white_point.y = 16450;
		md.hdmi_metadata_type1.max_display_mastering_luminance = 1000;
		md.hdmi_metadata_type1.min_display_mastering_luminance = 1;
		md.hdmi_metadata_type1.max_cll = 1000;
		md.hdmi_metadata_type1.max_fall = 400;
		printf("hdr metadata blob: %zu bytes\n", sizeof(md));
		if (drmModeCreatePropertyBlob(fd, &md, sizeof(md), &hdr_blob))
			printf("hdr metadata blob: %s\n", strerror(errno));
	}

	drmModeAtomicReq *req = drmModeAtomicAlloc();
	add(req, &cp, disp->connector_id, "CRTC_ID", crtc_id);
	if (o->hdr_out) {
		if (hdr_blob)
			add(req, &cp, disp->connector_id, "HDR_OUTPUT_METADATA", hdr_blob);
		add(req, &cp, disp->connector_id, "Colorspace", 10);   /* BT2020_YCC */
		add(req, &cp, disp->connector_id, "color_depth", 10);
		add(req, &cp, disp->connector_id, "color_format", 1);  /* ycbcr444 */
	} else {
		/* Connector properties survive between processes, so an earlier
		 * HDR run leaves the output in HDR10 and a later "SDR" run would
		 * silently measure the HDR pipeline. The SDR case has to be
		 * asserted, not merely left unset. */
		add(req, &cp, disp->connector_id, "HDR_OUTPUT_METADATA", 0);
		add(req, &cp, disp->connector_id, "Colorspace", 0);    /* DEFAULT */
	}
	add(req, &rp, crtc_id, "MODE_ID", mode_blob);
	add(req, &rp, crtc_id, "ACTIVE", 1);

	add(req, &vp, vid_plane, "FB_ID", vfb.fb_id);
	add(req, &vp, vid_plane, "CRTC_ID", crtc_id);
	add(req, &vp, vid_plane, "SRC_X", 0);
	add(req, &vp, vid_plane, "SRC_Y", 0);
	uint32_t sw = o->src_w ?: mode->hdisplay, sh = o->src_h ?: mode->vdisplay;
	uint32_t dw = o->dst_w ?: mode->hdisplay, dh = o->dst_h ?: mode->vdisplay;
	add(req, &vp, vid_plane, "SRC_W", (uint64_t)sw << 16);
	add(req, &vp, vid_plane, "SRC_H", (uint64_t)sh << 16);
	add(req, &vp, vid_plane, "CRTC_X", o->dst_x);
	add(req, &vp, vid_plane, "CRTC_Y", o->dst_y);
	add(req, &vp, vid_plane, "CRTC_W", dw);
	add(req, &vp, vid_plane, "CRTC_H", dh);
	printf("video format: %s\n", o->nv12 ? "NV12 (YUV)" : "AR24 (RGB)");
	printf("video rect: src %ux%u -> dst %ux%u+%u+%u%s\n", sw, sh, dw, dh,
	       o->dst_x, o->dst_y,
	       (sw != dw || sh != dh) ? " (scaler engaged)" : "");
	if (o->hdr) {
		add(req, &vp, vid_plane, "EOTF", 2);
		add(req, &vp, vid_plane, "COLOR_ENCODING", 2);  /* BT.2020 YCbCr */
		add(req, &vp, vid_plane, "COLOR_RANGE", 0);     /* limited */
	}

	if (o->gui) {
		add(req, &gp, gui_plane, "FB_ID", gfb.fb_id);
		add(req, &gp, gui_plane, "CRTC_ID", crtc_id);
		add(req, &gp, gui_plane, "SRC_X", 0);
		add(req, &gp, gui_plane, "SRC_Y", 0);
		add(req, &gp, gui_plane, "SRC_W", (uint64_t)mode->hdisplay << 16);
		add(req, &gp, gui_plane, "SRC_H", (uint64_t)mode->vdisplay << 16);
		add(req, &gp, gui_plane, "CRTC_X", 0);
		add(req, &gp, gui_plane, "CRTC_Y", 0);
		add(req, &gp, gui_plane, "CRTC_W", mode->hdisplay);
		add(req, &gp, gui_plane, "CRTC_H", mode->vdisplay);
		if (o->gui_eotf >= 0)
			add(req, &gp, gui_plane, "EOTF", o->gui_eotf);
		else if (o->hdr)
			add(req, &gp, gui_plane, "EOTF", 2);
	} else {
		add(req, &gp, gui_plane, "FB_ID", 0);
		add(req, &gp, gui_plane, "CRTC_ID", 0);
	}

	if (drmModeAtomicCommit(fd, req, DRM_MODE_ATOMIC_ALLOW_MODESET, NULL))
		die("atomic commit: %s", strerror(errno));
	drmModeAtomicFree(req);
	printf("committed: gui_layer=%s hdr=%d\n", o->gui ? "on" : "off", o->hdr);

	if (o->wb_path && wb && o->toggle <= 0)
		writeback_once(fd, wb, crtc_id, mode, o->wb_path, NULL);
	else if (o->wb_path && !wb)
		printf("writeback: no writeback connector found\n");

	/* Toggling inside one master session, with no modeset, is what makes
	 * this comparable to Kodi: the OSD plane joins and leaves a live CRTC
	 * exactly the same way, so anything that moves here moves for reasons
	 * below the player. */
	if (o->toggle > 0) {
		for (int cycle = 0; cycle < o->toggle; cycle++) {
			for (int on = 0; on < 2; on++) {
				drmModeAtomicReq *t = drmModeAtomicAlloc();
				if (on) {
					add(t, &gp, gui_plane, "FB_ID", gfb.fb_id);
					add(t, &gp, gui_plane, "CRTC_ID", crtc_id);
					add(t, &gp, gui_plane, "SRC_X", 0);
					add(t, &gp, gui_plane, "SRC_Y", 0);
					add(t, &gp, gui_plane, "SRC_W", (uint64_t)mode->hdisplay << 16);
					add(t, &gp, gui_plane, "SRC_H", (uint64_t)mode->vdisplay << 16);
					add(t, &gp, gui_plane, "CRTC_X", 0);
					add(t, &gp, gui_plane, "CRTC_Y", 0);
					add(t, &gp, gui_plane, "CRTC_W", mode->hdisplay);
					add(t, &gp, gui_plane, "CRTC_H", mode->vdisplay);
					if (o->gui_eotf >= 0)
						add(t, &gp, gui_plane, "EOTF", o->gui_eotf);
					else if (o->hdr)
						add(t, &gp, gui_plane, "EOTF", 2);
				} else {
					add(t, &gp, gui_plane, "FB_ID", 0);
					add(t, &gp, gui_plane, "CRTC_ID", 0);
				}
				if (drmModeAtomicCommit(fd, t, 0, NULL))
					printf("toggle commit (%s) failed: %s\n",
					       on ? "on" : "off", strerror(errno));
				else
					printf("[cycle %d] gui_layer=%s\n", cycle, on ? "ON " : "off");
				drmModeAtomicFree(t);
				fflush(stdout);
				sleep(o->hold);
				if (o->wb_path && wb) {
					char path[512];
					struct layer_state ls = {
						.gp = &gp, .gui_plane = gui_plane,
						.gui_fb = gfb.fb_id, .crtc_id = crtc_id,
						.w = mode->hdisplay, .h = mode->vdisplay,
						.on = on, .hdr = o->hdr,
						.gui_eotf = o->gui_eotf,
					};
					snprintf(path, sizeof(path), "%s.c%d.%s.nv12",
						 o->wb_path, cycle, on ? "gui" : "novideo");
					writeback_once(fd, wb, crtc_id, mode, path, &ls);
				}
			}
		}
	} else if (o->hold > 0) {
		printf("holding %d s\n", o->hold);
		fflush(stdout);
		sleep(o->hold);
	}
	close(fd);
	return 0;
}

int main(int argc, char **argv)
{
	const char *path = getenv("DRM_CARD") ?: "/dev/dri/card0";
	const char *cmd = argc > 1 ? argv[1] : "audit";
	if (!strcmp(cmd, "audit"))
		return cmd_audit(path);
	if (!strcmp(cmd, "run")) {
		struct opts o = { .card = path, .width = 3840, .height = 2160,
				  .refresh = 0, .hold = 10, .gui_eotf = -1 };
		for (int i = 2; i < argc; i++) {
			if (!strcmp(argv[i], "--gui")) o.gui = 1;
			else if (!strcmp(argv[i], "--hdr")) o.hdr = 1;
			else if (!strcmp(argv[i], "--nv12")) o.nv12 = 1;
			else if (!strcmp(argv[i], "--hdr-out")) o.hdr_out = 1;
			else if (!strcmp(argv[i], "--gui-eotf") && i + 1 < argc)
				o.gui_eotf = atoi(argv[++i]);
			else if (!strcmp(argv[i], "--gui-plane") && i + 1 < argc)
				o.gui_plane_override = strtoul(argv[++i], NULL, 0);
			else if (!strcmp(argv[i], "--hold") && i + 1 < argc) o.hold = atoi(argv[++i]);
			else if (!strcmp(argv[i], "--toggle") && i + 1 < argc) o.toggle = atoi(argv[++i]);
			else if (!strcmp(argv[i], "--src") && i + 1 < argc)
				sscanf(argv[++i], "%ux%u", &o.src_w, &o.src_h);
			else if (!strcmp(argv[i], "--dst") && i + 1 < argc)
				sscanf(argv[++i], "%ux%u+%u+%u", &o.dst_w, &o.dst_h, &o.dst_x, &o.dst_y);
			else if (!strcmp(argv[i], "--writeback") && i + 1 < argc) o.wb_path = argv[++i];
			else if (!strcmp(argv[i], "--mode") && i + 1 < argc)
				sscanf(argv[++i], "%ux%u@%u", &o.width, &o.height, &o.refresh);
			else die("unknown option %s", argv[i]);
		}
		return cmd_run(&o);
	}
	fprintf(stderr,
		"usage: %s audit\n"
		"       %s run [--gui] [--hdr] [--hdr-out] [--nv12] [--hold N] [--toggle CYCLES]\n"
		"                 [--mode WxH@R] [--src WxH] [--dst WxH+X+Y] [--writeback FILE]\n",
		argv[0], argv[0]);
	return 2;
}
