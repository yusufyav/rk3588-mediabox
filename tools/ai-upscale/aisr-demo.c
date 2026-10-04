/*
 * aisr-demo: MBSR against VOP2 on the television, live.
 *
 * Every frame goes through MBSR on the NPU (INT8, all three cores) into a
 * 3840x2160 NV12 buffer. The screen shows, alternating every AB seconds,
 * that buffer 1:1 (a white square top left marks it) or the same 1080p
 * frame scaled 2x by VOP2, as the product does today. Only Cluster0 and
 * Esmart0 can reach VP0 on the Plus and Cluster0 refuses NV12, so a split
 * screen over two planes is not possible: one plane, A/B in time.
 * Frames are paced at 1001/24 ms; the console prints per-second timings.
 *
 * Research tool, not part of the product. Needs DRM master: stop
 * mediabox-tv-ui first, start it again afterwards. No writeback is used.
 *
 * aisr-demo MODEL.rknn SRC.nv12 SECONDS [PLANE [AB_SECONDS]]
 *   SRC: one or more tightly packed 1920x1080 NV12 frames, looped
 *   AISR_IN_STD=255 as for aisr-bench
 */
#define AISR_LIB
#include "aisr-bench.c"

#include <fcntl.h>
#include <sys/mman.h>
#include <xf86drm.h>
#include <xf86drmMode.h>
#include <drm_fourcc.h>

struct fb { uint32_t id, pitch; uint8_t *map; };

static void fb_nv12(int fd, uint32_t w, uint32_t h, struct fb *f)
{
	struct drm_mode_create_dumb c = { .width = w, .height = h * 3 / 2, .bpp = 8 };
	if (drmIoctl(fd, DRM_IOCTL_MODE_CREATE_DUMB, &c)) die("create dumb", errno);
	uint32_t hd[4] = { c.handle, c.handle }, pi[4] = { c.pitch, c.pitch }, of[4] = { 0, c.pitch * h };
	if (drmModeAddFB2(fd, w, h, DRM_FORMAT_NV12, hd, pi, of, &f->id, 0)) die("addfb", errno);
	struct drm_mode_map_dumb m = { .handle = c.handle };
	if (drmIoctl(fd, DRM_IOCTL_MODE_MAP_DUMB, &m)) die("map dumb", errno);
	f->map = mmap(0, c.size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, m.offset);
	if (f->map == MAP_FAILED) die("mmap", errno);
	f->pitch = c.pitch;
}

static uint32_t propid(int fd, uint32_t obj, const char *name, uint64_t *ev, const char *en)
{
	drmModeObjectProperties *p = drmModeObjectGetProperties(fd, obj, DRM_MODE_OBJECT_PLANE);
	uint32_t id = 0;
	for (uint32_t i = 0; p && i < p->count_props; i++) {
		drmModePropertyRes *r = drmModeGetProperty(fd, p->props[i]);
		if (r && !strcmp(r->name, name)) {
			id = r->prop_id;
			for (int e = 0; en && e < r->count_enums; e++)
				if (!strcmp(r->enums[e].name, en)) *ev = r->enums[e].value;
		}
		drmModeFreeProperty(r);
	}
	drmModeFreeObjectProperties(p);
	return id;
}

static void set(drmModeAtomicReq *q, int fd, uint32_t pl, const char *n, uint64_t v)
{
	uint32_t id = propid(fd, pl, n, NULL, NULL);
	if (id) drmModeAtomicAddProperty(q, pl, id, v);
}

static void place(drmModeAtomicReq *q, int fd, uint32_t pl, uint32_t crtc, uint32_t fb,
		  uint32_t sx, uint32_t sy, uint32_t sw, uint32_t sh, uint32_t dx, uint32_t dw, uint32_t dh)
{
	uint64_t enc = 0, rng = 0;
	uint32_t pe = propid(fd, pl, "COLOR_ENCODING", &enc, "ITU-R BT.709 YCbCr");
	uint32_t pr = propid(fd, pl, "COLOR_RANGE", &rng, "YCbCr limited range");
	set(q, fd, pl, "FB_ID", fb); set(q, fd, pl, "CRTC_ID", crtc);
	set(q, fd, pl, "SRC_X", (uint64_t)sx << 16); set(q, fd, pl, "SRC_Y", (uint64_t)sy << 16);
	set(q, fd, pl, "SRC_W", (uint64_t)sw << 16); set(q, fd, pl, "SRC_H", (uint64_t)sh << 16);
	set(q, fd, pl, "CRTC_X", dx); set(q, fd, pl, "CRTC_Y", 0);
	set(q, fd, pl, "CRTC_W", dw); set(q, fd, pl, "CRTC_H", dh);
	if (pe) drmModeAtomicAddProperty(q, pl, pe, enc);
	if (pr) drmModeAtomicAddProperty(q, pl, pr, rng);
}

int main(int argc, char **argv)
{
	if (argc < 4) {
		fprintf(stderr, "usage: %s MODEL.rknn SRC.nv12 SECONDS [AI_PLANE VOP2_PLANE [swap]]\n", argv[0]);
		return 2;
	}
	double secs = atof(argv[3]);
	uint32_t ai_pl = argc > 4 ? atoi(argv[4]) : 73;
	double ab = argc > 5 ? atof(argv[5]) : 5.0;

	/* source frames */
	FILE *fp = fopen(argv[2], "rb");
	if (!fp) die("open src", errno);
	fseek(fp, 0, SEEK_END);
	long sz = ftell(fp);
	fseek(fp, 0, SEEK_SET);
	int nframes = sz / (SW * SH * 3 / 2);
	uint8_t *frames = malloc(sz);
	if (fread(frames, 1, sz, fp) != (size_t)sz) die("read src", errno);
	fclose(fp);

	/* NPU, as aisr-bench */
	fp = fopen(argv[1], "rb");
	if (!fp) die("open model", errno);
	fseek(fp, 0, SEEK_END);
	long msz = ftell(fp);
	fseek(fp, 0, SEEK_SET);
	void *mbuf = malloc(msz);
	if (fread(mbuf, 1, msz, fp) != (size_t)msz) die("read model", errno);
	fclose(fp);
	rknn_context ctx;
	int ret = rknn_init(&ctx, mbuf, msz, 0, NULL);
	if (ret) die("rknn_init", ret);
	rknn_set_core_mask(ctx, RKNN_NPU_CORE_0_1_2);
	struct ctx c = { .nv12 = 1, .nthreads = 4 };
	c.in.index = 0;
	rknn_query(ctx, RKNN_QUERY_NATIVE_INPUT_ATTR, &c.in, sizeof(c.in));
	c.out.index = 0;
	rknn_query(ctx, RKNN_QUERY_NATIVE_OUTPUT_ATTR, &c.out, sizeof(c.out));
	float in_std = getenv("AISR_IN_STD") ? atof(getenv("AISR_IN_STD")) : 1.0f;
	for (int q = 0; q < 256; q++) {
		long v = lrintf(q / in_std / c.in.scale) + c.in.zp;
		c.qin[q] = v < -128 ? -128 : v > 127 ? 127 : v;
		c.lut[q] = clamp_y(((int8_t)q - c.out.zp) * c.out.scale);
	}
	c.in.pass_through = 1;
	c.min = rknn_create_mem(ctx, c.in.size_with_stride);
	c.mout = rknn_create_mem(ctx, c.out.size_with_stride);
	rknn_set_io_mem(ctx, c.min, &c.in);
	rknn_set_io_mem(ctx, c.mout, &c.out);
	c.dst = aligned_alloc(64, (size_t)DW * DH * 3 / 2);

	/* display */
	int fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
	drmSetClientCap(fd, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1);
	if (drmSetClientCap(fd, DRM_CLIENT_CAP_ATOMIC, 1)) die("atomic", errno);
	if (!drmIsMaster(fd) && drmSetMaster(fd)) die("not DRM master: stop mediabox-tv-ui", errno);
	drmModeRes *res = drmModeGetResources(fd);
	drmModeConnector *disp = NULL;
	for (int i = 0; i < res->count_connectors && !disp; i++) {
		drmModeConnector *k = drmModeGetConnector(fd, res->connectors[i]);
		if (k->connection == DRM_MODE_CONNECTED && k->encoder_id && k->connector_type == DRM_MODE_CONNECTOR_HDMIA)
			disp = k;
	}
	if (!disp) die("no connected HDMI", 0);
	drmModeEncoder *enc = drmModeGetEncoder(fd, disp->encoder_id);
	drmModeCrtc *crtc = drmModeGetCrtc(fd, enc->crtc_id);
	uint32_t MW = crtc->mode.hdisplay, MH = crtc->mode.vdisplay;
	printf("display %ux%u@%u, plane %u, MBSR / VOP2 every %.0f s, MBSR first (white square)\n",
	       MW, MH, crtc->mode.vrefresh, ai_pl, ab);
	if (MW != DW || MH != DH) die("needs a 3840x2160 mode", 0);

	struct fb ai[2], src[2];
	for (int i = 0; i < 2; i++) {
		fb_nv12(fd, DW, DH, &ai[i]);
		fb_nv12(fd, SW, SH, &src[i]);
	}
	/* everything else on this CRTC off */
	drmModePlaneRes *pres = drmModeGetPlaneResources(fd);
	drmModeAtomicReq *q0 = drmModeAtomicAlloc();
	for (uint32_t i = 0; i < pres->count_planes; i++) {
		drmModePlane *pl = drmModeGetPlane(fd, pres->planes[i]);
		if (pl->crtc_id == crtc->crtc_id && pl->plane_id != ai_pl) {
			set(q0, fd, pl->plane_id, "FB_ID", 0);
			set(q0, fd, pl->plane_id, "CRTC_ID", 0);
		}
		drmModeFreePlane(pl);
	}
	if (drmModeAtomicCommit(fd, q0, DRM_MODE_ATOMIC_ALLOW_MODESET, NULL)) die("clear planes", errno);
	drmModeAtomicFree(q0);

	const double period = 1001.0 / 24.0;
	double t0 = now_ms(), next = t0, sec_t = t0, worst = 0, sum = 0;
	int k = 0, shown = 0, late = 0, n_sec = 0;
	while (now_ms() - t0 < secs * 1000) {
		int b = k & 1;
		c.src = frames + (size_t)(k % nframes) * SW * SH * 3 / 2;
		double a = now_ms();
		par(&c, pre_rows, SH / 2);
		rknn_mem_sync(ctx, c.min, RKNN_MEMORY_SYNC_TO_DEVICE);
		if ((ret = rknn_run(ctx, NULL))) die("rknn_run", ret);
		rknn_mem_sync(ctx, c.mout, RKNN_MEMORY_SYNC_FROM_DEVICE);
		par(&c, post_rows, c.out.dims[2]);
		double e2e = now_ms() - a;
		/* into scan-out buffers */
		for (uint32_t y = 0; y < DH * 3 / 2; y++)
			memcpy(ai[b].map + (size_t)y * ai[b].pitch, c.dst + (size_t)y * DW, DW);
		for (uint32_t y = 40; y < 140; y++)             /* the MBSR marker */
			memset(ai[b].map + (size_t)y * ai[b].pitch + 40, 235, 100);
		for (uint32_t y = 0; y < SH * 3 / 2; y++)
			memcpy(src[b].map + (size_t)y * src[b].pitch, c.src + (size_t)y * SW, SW);
		double busy = now_ms() - a;
		/* pace to 23.976 */
		next += period;
		double w = next - now_ms();
		if (w > 0)
			usleep((useconds_t)(w * 1000));
		else
			late++;
		drmModeAtomicReq *q = drmModeAtomicAlloc();
		int show_ai = ((int)((now_ms() - t0) / 1000 / ab)) % 2 == 0;
		if (show_ai)
			place(q, fd, ai_pl, crtc->crtc_id, ai[b].id, 0, 0, DW, DH, 0, DW, DH);
		else
			place(q, fd, ai_pl, crtc->crtc_id, src[b].id, 0, 0, SW, SH, 0, DW, DH);
		if (drmModeAtomicCommit(fd, q, DRM_MODE_ATOMIC_NONBLOCK | DRM_MODE_ATOMIC_ALLOW_MODESET, NULL)
		    && drmModeAtomicCommit(fd, q, DRM_MODE_ATOMIC_ALLOW_MODESET, NULL))
			die("commit", errno);
		drmModeAtomicFree(q);
		k++;
		shown++;
		sum += e2e;
		n_sec++;
		worst = e2e > worst ? e2e : worst;
		if (now_ms() - sec_t >= 1000) {
			printf("%5.0f s  %2d frames  AI e2e mean %5.1f ms worst %5.1f ms  (frame+copy %5.1f ms)  late %d\n",
			       (now_ms() - t0) / 1000, n_sec, sum / n_sec, worst, busy, late);
			fflush(stdout);
			sec_t = now_ms();
			sum = worst = 0;
			n_sec = 0;
		}
	}
	printf("shown %d frames, %d late\n", shown, late);
	rknn_destroy(ctx);
	return 0;
}
