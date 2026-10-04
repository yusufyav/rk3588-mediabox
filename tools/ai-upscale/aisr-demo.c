/*
 * aisr-demo: MBSR against VOP2 on the television, live, under the viewer's control.
 *
 * Frames come from a file or from stdin (any decoder piping 1920x1080 NV12,
 * e.g. ffmpeg with the RKMPP decoder on a film of the viewer's choosing).
 * The screen shows one of:
 *   ai    the frame through MBSR on the NPU (INT8, three cores) into a
 *         3840x2160 buffer, scanned out 1:1; a white square marks it
 *   vop2  the same 1080p frame scaled 2x by VOP2, as the product does today;
 *         the NPU is not run at all, so its load drops to zero
 *   ab N  alternate every N seconds
 * and "zoom Z X Y" magnifies the same region by Z for both (X, Y in 0..1),
 * so the comparison is of the upscale, not of the eye's acuity.
 * The mode is read from MODEFILE (default /tmp/aisr/mode) while it runs.
 *
 * Only Cluster0 and Esmart0 reach VP0 on the Plus and Cluster0 refuses NV12,
 * so a split screen over two planes is not possible: one plane, in time.
 *
 * Research tool, not part of the product. Needs DRM master: stop
 * mediabox-tv-ui first, start it again afterwards. No writeback is used.
 *
 * aisr-demo MODEL.rknn SRC.nv12|- SECONDS [PLANE [MODEFILE]]
 *   AISR_IN_STD=255 as for aisr-bench
 *   AISR_SRC_P010=WxH: stdin carries 10-bit HDR10 (PQ) yuv420p10le frames of WxH
 *     (planar, low bits; this ffmpeg's nv15 -> p010le conversion is broken);
 *     they are tone-mapped to SDR BT.709 NV12 and centred in 1920x1080 before
 *     either path sees them. The mapping is a demo approximation (Y' as the
 *     luminance proxy, 203-nit reference white, soft shoulder, chroma scaled
 *     with luma, no BT.2020->709 matrix); MBSR and VOP2 get the same frame.
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

/* ---- HDR10 P010 -> SDR NV12, for the demo only ---- */
static uint8_t tm_y[1024];
static float tm_s[1024];
static const uint16_t *p010;
static int pw, ph;

static void tm_init(void)
{
	const double m1 = 0.1593017578125, m2 = 78.84375, c1 = 0.8359375, c2 = 18.8515625, c3 = 18.6875;
	for (int v = 0; v < 1024; v++) {
		double e = (v - 64) / 876.0;
		e = e < 0 ? 0 : e > 1 ? 1 : e;
		double p = pow(e, 1 / m2), L = 10000 * pow(fmax(p - c1, 0) / (c2 - c3 * p), 1 / m1);
		double ls = L / 203.0, o = ls <= 0.8 ? ls : 0.8 + 0.2 * (1 - exp(-(ls - 0.8) / 0.2));
		double V = pow(o, 1 / 2.4);
		tm_y[v] = (uint8_t)lrint(16 + 219 * V);
		tm_s[v] = e > 0.02 ? fmin(V / e, 2.5) : 1.0f;
	}
}

static void tm_rows(struct ctx *c, int r0, int r1)
{
	uint8_t *d = (uint8_t *)c->src;                     /* the NV12 frame being filled */
	int ox = (SW - pw) / 2 & ~1, oy = (SH - ph) / 2 & ~1;
	for (int r = r0; r < r1; r++) {                      /* rows of chroma: 2 luma rows each */
		for (int k = 0; k < 2; k++) {
			int y = 2 * r + k;
			uint8_t *o = d + (size_t)y * SW;
			if (y < oy || y >= oy + ph) { memset(o, 16, SW); continue; }
			const uint16_t *s = p010 + (size_t)(y - oy) * pw;
			memset(o, 16, ox);
			for (int x = 0; x < pw; x++)
				o[ox + x] = tm_y[s[x] & 1023];
			memset(o + ox + pw, 16, SW - ox - pw);
		}
		uint8_t *o = d + (size_t)SW * SH + (size_t)r * SW;
		int cy = r - oy / 2;
		if (cy < 0 || cy >= ph / 2) { memset(o, 128, SW); continue; }
		const uint16_t *su = p010 + (size_t)pw * ph + (size_t)cy * (pw / 2);
		const uint16_t *sv = su + (size_t)(pw / 2) * (ph / 2);
		const uint16_t *yy = p010 + (size_t)(2 * cy) * pw;
		memset(o, 128, ox);
		for (int x = 0; x < pw / 2; x++) {
			float f = tm_s[yy[2 * x] & 1023] * 0.25f;
			int u = (int)lrintf(128 + ((su[x] & 1023) - 512) * f);
			int v = (int)lrintf(128 + ((sv[x] & 1023) - 512) * f);
			o[ox + 2 * x] = u < 16 ? 16 : u > 240 ? 240 : u;
			o[ox + 2 * x + 1] = v < 16 ? 16 : v > 240 ? 240 : v;
		}
		memset(o + ox + pw, 128, SW - ox - pw);
	}
}

int main(int argc, char **argv)
{
	if (argc < 4) {
		fprintf(stderr, "usage: %s MODEL.rknn SRC.nv12 SECONDS [AI_PLANE VOP2_PLANE [swap]]\n", argv[0]);
		return 2;
	}
	double secs = atof(argv[3]);
	uint32_t ai_pl = argc > 4 ? atoi(argv[4]) : 73;
	const char *modefile = argc > 5 ? argv[5] : "/tmp/aisr/mode";

	/* source frames: a file (looped) or stdin (streamed) */
	const size_t FS = (size_t)SW * SH * 3 / 2;
	int live = !strcmp(argv[2], "-");
	FILE *fp;
	int nframes = 1;
	uint8_t *frames;
	const char *p010env = getenv("AISR_SRC_P010");
	uint16_t *p010buf = NULL;
	size_t PS = 0;
	if (p010env && sscanf(p010env, "%dx%d", &pw, &ph) == 2) {
		PS = (size_t)pw * ph * 3;                       /* bytes: Y + UV, 16 bit each */
		p010buf = malloc(PS);
		p010 = p010buf;
		tm_init();
		printf("source: HDR10 P010 %dx%d, tone-mapped to SDR NV12 for both paths\n", pw, ph);
	}
	if (live && p010buf && getenv("AISR_DUMP")) {
		/* AISR_DUMP=FILE N: the N-th tone-mapped frame to FILE, no display */
		char path[256];
		int skip = 0;
		sscanf(getenv("AISR_DUMP"), "%255s %d", path, &skip);
		frames = malloc(FS);
		struct ctx t = { .nthreads = 4, .src = frames };
		for (int i = 0; i <= skip; i++)
			if (fread(p010buf, 1, PS, stdin) != PS)
				die("short stdin", 0);
		par(&t, tm_rows, SH / 2);
		FILE *o = fopen(path, "wb");
		fwrite(frames, 1, FS, o);
		fclose(o);
		printf("dumped frame %d to %s\n", skip, path);
		return 0;
	}
	if (live) {
		frames = malloc(FS);
	} else {
		fp = fopen(argv[2], "rb");
		if (!fp) die("open src", errno);
		fseek(fp, 0, SEEK_END);
		long sz = ftell(fp);
		fseek(fp, 0, SEEK_SET);
		nframes = sz / FS;
		frames = malloc(sz);
		if (fread(frames, 1, sz, fp) != (size_t)sz) die("read src", errno);
		fclose(fp);
	}

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
	printf("display %ux%u@%u, plane %u, mode file %s (ai | vop2 | ab N) [zoom Z X Y]\n",
	       MW, MH, crtc->mode.vrefresh, ai_pl, modefile);
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
	int k = 0, shown = 0, late = 0, n_sec = 0, n_ai = 0;
	char mode[16] = "ai";
	double ab = 0, zoom = 1, zx = 0.5, zy = 0.5;
	while (now_ms() - t0 < secs * 1000) {
		if (k % 6 == 0) {                       /* the viewer's choice, 4x a second */
			FILE *mf = fopen(modefile, "r");
			char line[128] = "";
			if (mf && fgets(line, sizeof(line), mf)) {
				char m[16] = "", z[8] = "";
				double a1 = 0, a2 = 1, a3 = 0.5, a4 = 0.5;
				int n = sscanf(line, "%15s %lf %7s %lf %lf %lf", m, &a1, z, &a2, &a3, &a4);
				if (!strcmp(m, "ab")) { strcpy(mode, "ab"); ab = a1 > 0 ? a1 : 5; }
				else if (!strcmp(m, "ai") || !strcmp(m, "vop2")) {
					strcpy(mode, m);
					/* "ai zoom 4 0.5 0.5": no number after the mode */
					n = sscanf(line, "%15s %7s %lf %lf %lf", m, z, &a2, &a3, &a4);
					if (n < 2) z[0] = 0;
				}
				if (!strcmp(z, "zoom") && a2 >= 1) { zoom = a2; zx = a3; zy = a4; }
				else zoom = 1;
			}
			if (mf)
				fclose(mf);
		}
		int show_ai = !strcmp(mode, "ai") || (!strcmp(mode, "ab") && ((int)((now_ms() - t0) / 1000 / ab)) % 2 == 0);
		int b = k & 1;
		if (live && p010buf) {
			if (fread(p010buf, 1, PS, stdin) != PS)
				break;
			c.src = frames;
			par(&c, tm_rows, SH / 2);
		} else if (live) {
			if (fread(frames, 1, FS, stdin) != FS)
				break;
			c.src = frames;
		} else {
			c.src = frames + (size_t)(k % nframes) * FS;
		}
		double a = now_ms(), e2e = 0;
		/* the visible region, the same for both paths */
		uint32_t cw = (uint32_t)(DW / zoom) & ~3u, ch = (uint32_t)(DH / zoom) & ~3u;
		int cx = (int)(zx * DW) - (int)cw / 2, cy = (int)(zy * DH) - (int)ch / 2;
		cx = cx < 0 ? 0 : cx > (int)(DW - cw) ? (int)(DW - cw) : cx;
		cy = cy < 0 ? 0 : cy > (int)(DH - ch) ? (int)(DH - ch) : cy;
		cx &= ~3; cy &= ~3;
		if (show_ai) {
			par(&c, pre_rows, SH / 2);
			rknn_mem_sync(ctx, c.min, RKNN_MEMORY_SYNC_TO_DEVICE);
			if ((ret = rknn_run(ctx, NULL))) die("rknn_run", ret);
			rknn_mem_sync(ctx, c.mout, RKNN_MEMORY_SYNC_FROM_DEVICE);
			par(&c, post_rows, c.out.dims[2]);
			e2e = now_ms() - a;
			for (uint32_t y = 0; y < DH * 3 / 2; y++)
				memcpy(ai[b].map + (size_t)y * ai[b].pitch, c.dst + (size_t)y * DW, DW);
			uint32_t m0 = 10 + (uint32_t)(30 / zoom), ms = (uint32_t)(100 / zoom) < 8 ? 8 : (uint32_t)(100 / zoom);
			for (uint32_t y = cy + m0; y < cy + m0 + ms; y++)      /* the MBSR marker */
				memset(ai[b].map + (size_t)y * ai[b].pitch + cx + m0, 235, ms);
			n_ai++;
		} else {
			for (uint32_t y = 0; y < SH * 3 / 2; y++)
				memcpy(src[b].map + (size_t)y * src[b].pitch, c.src + (size_t)y * SW, SW);
		}
		next += period;
		double w = next - now_ms();
		if (w > 0)
			usleep((useconds_t)(w * 1000));
		else {
			late++;
			if (w < -period)
				next = now_ms();                /* a stalled source: do not race */
		}
		drmModeAtomicReq *q = drmModeAtomicAlloc();
		if (show_ai)
			place(q, fd, ai_pl, crtc->crtc_id, ai[b].id, cx, cy, cw, ch, 0, DW, DH);
		else
			place(q, fd, ai_pl, crtc->crtc_id, src[b].id, cx / 2, cy / 2, cw / 2, ch / 2, 0, DW, DH);
		if (drmModeAtomicCommit(fd, q, DRM_MODE_ATOMIC_NONBLOCK | DRM_MODE_ATOMIC_ALLOW_MODESET, NULL)
		    && drmModeAtomicCommit(fd, q, DRM_MODE_ATOMIC_ALLOW_MODESET, NULL))
			die("commit", errno);
		drmModeAtomicFree(q);
		k++;
		shown++;
		n_sec++;
		if (show_ai) {
			sum += e2e;
			worst = e2e > worst ? e2e : worst;
		}
		if (now_ms() - sec_t >= 1000) {
			printf("%5.0f s  %2d frames  showing %-4s zoom %.0fx  MBSR e2e mean %5.1f ms worst %5.1f ms  late %d\n",
			       (now_ms() - t0) / 1000, n_sec, show_ai ? "AI" : "VOP2", zoom,
			       n_ai ? sum / n_ai : 0.0, worst, late);
			fflush(stdout);
			sec_t = now_ms();
			sum = worst = 0;
			n_sec = n_ai = 0;
		}
	}
	printf("shown %d frames, %d late\n", shown, late);
	rknn_destroy(ctx);
	return 0;
}
