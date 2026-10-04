/*
 * aisr-bench: what a 1080p -> 2160p luma SR model really costs per frame on
 * the RK3588 NPU, from an NV12 frame in to an NV12 frame out.
 *
 *   pre   Y plane -> the model's native input tensor (CPU, zero-copy memory)
 *   npu   rknn_run; the driver's own figure is reported next to it
 *   post  native output tensor -> 3840x2160 Y (dequantise + pixel shuffle, CPU)
 *   uv    960x540 UV -> 1920x1080 UV, bilinear (CPU); in nv12 mode the
 *         model makes the chroma itself and this stage is empty
 *   e2e   all of the above, one frame after the other
 *
 * Research tool. Not part of the product, not installed, not run by it.
 *
 * aisr-bench MODEL plain|s2d|nv12 CORE_MASK SRC.nv12 [-w warmup] [-n iters]
 *            [-t threads] [-o out.nv12] [-s seconds] [-l log.csv]
 * nv12: 6 channels in (Y of each 2x2 block, U, V at 540x960), 24 out (Y of
 * the 4x4 block, U and V of its 2x2 chroma samples) -- rt4ksr.py's graph.
 * CORE_MASK: 0 auto, 1/2/4 one core, 3 cores 0+1, 7 cores 0+1+2.
 * SRC may hold several 1920x1080 frames; with -o each is converted once
 * after the timed run and written out (quality and temporal tests).
 */
#define _GNU_SOURCE
#include <errno.h>
#include <math.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "rknn_api.h"

#define SW 1920
#define SH 1080
#define DW 3840
#define DH 2160

static double now_ms(void)
{
	struct timespec t;
	clock_gettime(CLOCK_MONOTONIC, &t);
	return t.tv_sec * 1e3 + t.tv_nsec / 1e6;
}

static void die(const char *what, int ret)
{
	fprintf(stderr, "%s failed: %d\n", what, ret);
	exit(1);
}

struct ctx {
	int s2d;                        /* 1: 4ch 540x960 in, 16ch out */
	int nv12;                       /* 1: 6ch 540x960 in, 24ch out */
	rknn_tensor_attr in, out;       /* native attrs */
	rknn_tensor_mem *min, *mout;
	uint8_t lut[256];               /* int8 output -> Y */
	int8_t qin[256];                /* Y -> int8 input */
	const uint8_t *src;             /* NV12 1080p */
	uint8_t *dst;                   /* NV12 2160p */
	int nthreads;
};

/* ---------- stages, each split over rows ---------- */

/* element offset of (channel, row, col) in a native NHWC or NC1HWC2 tensor */
static inline size_t nat(const rknn_tensor_attr *a, int ch, int r, int x)
{
	if (a->fmt == RKNN_TENSOR_NC1HWC2) {
		size_t H = a->dims[2], W = a->dims[3], C2 = a->dims[4];
		return (ch / C2) * H * W * C2 + ((size_t)r * W + x) * C2 + ch % C2;
	}
	size_t W = a->dims[2], C = a->dims[3];
	return ((size_t)r * W + x) * C + ch;
}

static void pre_nv12(struct ctx *c, int r0, int r1)
{
	rknn_tensor_attr *a = &c->in;
	int f16 = a->type == RKNN_TENSOR_FLOAT16;
	const uint8_t *uv = c->src + SW * SH;
	for (int r = r0; r < r1; r++) {
		const uint8_t *s0 = c->src + (size_t)(2 * r) * SW, *s1 = s0 + SW, *su = uv + (size_t)r * SW;
		for (int x = 0; x < SW / 2; x++) {
			uint8_t v[6] = { s0[2 * x], s0[2 * x + 1], s1[2 * x], s1[2 * x + 1], su[2 * x], su[2 * x + 1] };
			for (int ch = 0; ch < 6; ch++) {
				size_t o = nat(a, ch, r, x);
				if (f16)
					((__fp16 *)c->min->virt_addr)[o] = v[ch];
				else
					((int8_t *)c->min->virt_addr)[o] = c->qin[v[ch]];
			}
		}
	}
}

static void pre_rows(struct ctx *c, int r0, int r1)
{
	if (c->nv12) {
		pre_nv12(c, r0, r1);
		return;
	}
	const uint8_t *y = c->src;
	rknn_tensor_attr *a = &c->in;
	int f16 = a->type == RKNN_TENSOR_FLOAT16;
	if (!c->s2d) {
		/* NHWC 1 channel, or NC1HWC2 with C2 lanes and only lane 0 used */
		int lanes = a->fmt == RKNN_TENSOR_NC1HWC2 ? a->dims[4] : 1;
		int ws = a->w_stride ? a->w_stride : SW;
		for (int r = r0; r < r1; r++) {
			const uint8_t *s = y + (size_t)r * SW;
			if (f16) {
				__fp16 *d = (__fp16 *)c->min->virt_addr + (size_t)r * ws * lanes;
				for (int x = 0; x < SW; x++)
					d[x * lanes] = s[x];
			} else {
				int8_t *d = (int8_t *)c->min->virt_addr + (size_t)r * ws * lanes;
				for (int x = 0; x < SW; x++)
					d[x * lanes] = c->qin[s[x]];
			}
		}
		return;
	}
	/* s2d: channel 2*pi+pj of half-res pixel (R,X) = Y[2R+pi][2X+pj] */
	int lanes = a->fmt == RKNN_TENSOR_NC1HWC2 ? a->dims[4] : 4;
	int ws = a->w_stride ? a->w_stride : SW / 2;
	for (int r = r0; r < r1; r++) {
		const uint8_t *s0 = y + (size_t)(2 * r) * SW, *s1 = s0 + SW;
		if (f16) {
			__fp16 *d = (__fp16 *)c->min->virt_addr + (size_t)r * ws * lanes;
			for (int x = 0; x < SW / 2; x++, d += lanes) {
				d[0] = s0[2 * x]; d[1] = s0[2 * x + 1];
				d[2] = s1[2 * x]; d[3] = s1[2 * x + 1];
			}
		} else {
			int8_t *d = (int8_t *)c->min->virt_addr + (size_t)r * ws * lanes;
			for (int x = 0; x < SW / 2; x++, d += lanes) {
				d[0] = c->qin[s0[2 * x]]; d[1] = c->qin[s0[2 * x + 1]];
				d[2] = c->qin[s1[2 * x]]; d[3] = c->qin[s1[2 * x + 1]];
			}
		}
	}
}

static inline uint8_t clamp_y(float v)
{
	v = v < 0.f ? 0.f : v > 255.f ? 255.f : v;
	return (uint8_t)(v + 0.5f);
}

static void post_nv12(struct ctx *c, int r0, int r1)
{
	rknn_tensor_attr *a = &c->out;
	int W = a->dims[3], C2 = a->dims[4];
	size_t plane = (size_t)a->dims[2] * W * C2;
	int f16 = a->type == RKNN_TENSOR_FLOAT16;
	uint8_t *duv = c->dst + (size_t)DW * DH;
	for (int r = r0; r < r1; r++) {
		for (int ch = 0; ch < 24; ch++) {
			uint8_t *o;
			int step;
			if (ch < 16) {
				o = c->dst + (size_t)(4 * r + ch / 4) * DW + ch % 4;
				step = 4;
			} else {
				int k = (ch - 16) % 4, comp = (ch - 16) / 4;
				o = duv + (size_t)(2 * r + k / 2) * DW + 2 * (k % 2) + comp;
				step = 4;
			}
			size_t base = (size_t)(ch / C2) * plane + (size_t)r * W * C2 + ch % C2;
			if (f16) {
				const __fp16 *s = (const __fp16 *)c->mout->virt_addr + base;
				for (int x = 0; x < W; x++)
					o[x * step] = clamp_y(s[(size_t)x * C2]);
			} else {
				const uint8_t *s = (const uint8_t *)c->mout->virt_addr + base;
				for (int x = 0; x < W; x++)
					o[x * step] = c->lut[s[(size_t)x * C2]];
			}
		}
	}
}

/* rows are rows of the output tensor (1080 plain, 540 s2d) */
static void post_rows(struct ctx *c, int r0, int r1)
{
	if (c->nv12) {
		post_nv12(c, r0, r1);
		return;
	}
	rknn_tensor_attr *a = &c->out;
	int f = c->s2d ? 4 : 2;         /* output block per tensor pixel */
	int C = f * f;
	int H = a->dims[2], W = a->dims[3], C2 = a->dims[4];
	size_t plane = (size_t)H * W * C2;
	int f16 = a->type == RKNN_TENSOR_FLOAT16;
	for (int r = r0; r < r1; r++) {
		for (int ch = 0; ch < C; ch++) {
			int dy = ch / f, dx = ch % f;
			uint8_t *o = c->dst + (size_t)(r * f + dy) * DW + dx;
			size_t base = (size_t)(ch / C2) * plane + (size_t)r * W * C2 + ch % C2;
			if (f16) {
				const __fp16 *s = (const __fp16 *)c->mout->virt_addr + base;
				for (int x = 0; x < W; x++)
					o[x * f] = clamp_y(s[(size_t)x * C2]);
			} else {
				const uint8_t *s = (const uint8_t *)c->mout->virt_addr + base;
				for (int x = 0; x < W; x++)
					o[x * f] = c->lut[s[(size_t)x * C2]];
			}
		}
	}
}

/* chroma: 960x540 -> 1920x1080, bilinear, centre-aligned; rows of the output */
static void uv_rows(struct ctx *c, int r0, int r1)
{
	const uint8_t *suv = c->src + SW * SH;
	uint8_t *duv = c->dst + (size_t)DW * DH;
	const int sw = SW / 2, sh = SH / 2;
	for (int r = r0; r < r1; r++) {
		int ry = r >> 1;
		int ry2 = (r & 1) ? (ry + 1 < sh ? ry + 1 : ry) : (ry > 0 ? ry - 1 : 0);
		const uint8_t *a = suv + (size_t)ry * SW, *b = suv + (size_t)ry2 * SW;
		uint8_t *o = duv + (size_t)r * DW;
		for (int x = 0; x < sw; x++) {
			int xl = x > 0 ? x - 1 : 0, xr = x + 1 < sw ? x + 1 : x;
			for (int k = 0; k < 2; k++) {
				int v = 3 * a[2 * x + k] + b[2 * x + k];
				int vl = 3 * a[2 * xl + k] + b[2 * xl + k];
				int vr = 3 * a[2 * xr + k] + b[2 * xr + k];
				o[4 * x + k] = (uint8_t)((3 * v + vl + 8) >> 4);
				o[4 * x + 2 + k] = (uint8_t)((3 * v + vr + 8) >> 4);
			}
		}
	}
}

/* ---------- a tiny fork/join pool ---------- */

typedef void (*stage_fn)(struct ctx *, int, int);
struct job { struct ctx *c; stage_fn fn; int r0, r1; };

static void *run_job(void *p)
{
	struct job *j = p;
	j->fn(j->c, j->r0, j->r1);
	return NULL;
}

static void par(struct ctx *c, stage_fn fn, int rows)
{
	int n = c->nthreads;
	if (n <= 1) {
		fn(c, 0, rows);
		return;
	}
	pthread_t th[16];
	struct job jobs[16];
	for (int i = 0; i < n; i++) {
		jobs[i] = (struct job){ c, fn, rows * i / n, rows * (i + 1) / n };
		if (i)
			pthread_create(&th[i], NULL, run_job, &jobs[i]);
	}
	run_job(&jobs[0]);
	for (int i = 1; i < n; i++)
		pthread_join(th[i], NULL);
}

static int cmp_d(const void *a, const void *b)
{
	double x = *(const double *)a, y = *(const double *)b;
	return x < y ? -1 : x > y;
}

static void stats(const char *name, double *v, int n)
{
	double *s = malloc(n * sizeof(*s)), sum = 0;
	memcpy(s, v, n * sizeof(*s));
	qsort(s, n, sizeof(*s), cmp_d);
	for (int i = 0; i < n; i++)
		sum += s[i];
	printf("%-5s min %7.2f  median %7.2f  p95 %7.2f  max %7.2f  mean %7.2f ms  (%6.1f fps)\n",
	       name, s[0], s[n / 2], s[(int)(n * 0.95)], s[n - 1], sum / n, 1000.0 * n / sum);
	free(s);
}

static void print_attr(const char *tag, rknn_tensor_attr *a)
{
	printf("%s: dims", tag);
	for (uint32_t i = 0; i < a->n_dims; i++)
		printf(" %u", a->dims[i]);
	printf(" fmt %s type %s qnt zp %d scale %g size %u w_stride %u size_with_stride %u\n",
	       get_format_string(a->fmt), get_type_string(a->type), a->zp, a->scale,
	       a->size, a->w_stride, a->size_with_stride);
}

int main(int argc, char **argv)
{
	if (argc < 5) {
		fprintf(stderr, "usage: %s MODEL plain|s2d CORE_MASK SRC.nv12 [-w N] [-n N] [-t N] [-o OUT] [-s SEC] [-l CSV]\n", argv[0]);
		return 2;
	}
	const char *model = argv[1], *srcpath = argv[4], *outpath = NULL, *logpath = NULL;
	int warm = 20, iters = 300, threads = 4, mask = atoi(argv[3]);
	double sustain = 0;
	struct ctx c = { .s2d = !strcmp(argv[2], "s2d"), .nv12 = !strcmp(argv[2], "nv12") };
	for (int i = 5; i + 1 < argc; i += 2) {
		if (!strcmp(argv[i], "-w")) warm = atoi(argv[i + 1]);
		else if (!strcmp(argv[i], "-n")) iters = atoi(argv[i + 1]);
		else if (!strcmp(argv[i], "-t")) threads = atoi(argv[i + 1]);
		else if (!strcmp(argv[i], "-o")) outpath = argv[i + 1];
		else if (!strcmp(argv[i], "-s")) sustain = atof(argv[i + 1]);
		else if (!strcmp(argv[i], "-l")) logpath = argv[i + 1];
	}
	c.nthreads = threads;

	FILE *fp = fopen(srcpath, "rb");
	if (!fp) die("open src", errno);
	fseek(fp, 0, SEEK_END);
	long srcsz = ftell(fp);
	fseek(fp, 0, SEEK_SET);
	int nframes = srcsz / (SW * SH * 3 / 2);
	uint8_t *frames = malloc(srcsz);
	if (fread(frames, 1, srcsz, fp) != (size_t)srcsz) die("read src", errno);
	fclose(fp);
	c.src = frames;
	c.dst = aligned_alloc(64, (size_t)DW * DH * 3 / 2);
	memset(c.dst, 0, (size_t)DW * DH * 3 / 2);

	fp = fopen(model, "rb");
	if (!fp) die("open model", errno);
	fseek(fp, 0, SEEK_END);
	long msz = ftell(fp);
	fseek(fp, 0, SEEK_SET);
	void *mbuf = malloc(msz);
	if (fread(mbuf, 1, msz, fp) != (size_t)msz) die("read model", errno);
	fclose(fp);

	rknn_context ctx;
	/* AISR_PERF_DETAIL=1: per-op times from the runtime after the run */
	int detail = getenv("AISR_PERF_DETAIL") != NULL;
	int ret = rknn_init(&ctx, mbuf, msz, detail ? RKNN_FLAG_COLLECT_PERF_MASK : 0, NULL);
	if (ret) die("rknn_init", ret);
	rknn_sdk_version ver;
	rknn_query(ctx, RKNN_QUERY_SDK_VERSION, &ver, sizeof(ver));
	printf("api %s driver %s\n", ver.api_version, ver.drv_version);
	if ((ret = rknn_set_core_mask(ctx, (rknn_core_mask)mask))) die("rknn_set_core_mask", ret);

	rknn_input_output_num io;
	rknn_query(ctx, RKNN_QUERY_IN_OUT_NUM, &io, sizeof(io));
	c.in.index = 0;
	rknn_query(ctx, RKNN_QUERY_NATIVE_INPUT_ATTR, &c.in, sizeof(c.in));
	c.out.index = 0;
	rknn_query(ctx, RKNN_QUERY_NATIVE_OUTPUT_ATTR, &c.out, sizeof(c.out));
	print_attr("native in ", &c.in);
	print_attr("native out", &c.out);
	if (c.out.fmt != RKNN_TENSOR_NC1HWC2) {
		fprintf(stderr, "unexpected output layout\n");
		return 1;
	}
	rknn_mem_size ms;
	if (!rknn_query(ctx, RKNN_QUERY_MEM_SIZE, &ms, sizeof(ms)))
		printf("mem: weight %.1f MB internal %.1f MB dma total %.1f MB\n",
		       ms.total_weight_size / 1048576.0, ms.total_internal_size / 1048576.0,
		       ms.total_dma_allocated_size / 1048576.0);

	for (int q = 0; q < 256; q++) {
		if (c.in.type == RKNN_TENSOR_INT8) {
			long v = lrintf(q / c.in.scale) + c.in.zp;
			c.qin[q] = v < -128 ? -128 : v > 127 ? 127 : v;
		}
		c.lut[q] = clamp_y(((int8_t)q - c.out.zp) * c.out.scale);
	}

	c.in.pass_through = 1;
	c.min = rknn_create_mem(ctx, c.in.size_with_stride);
	c.mout = rknn_create_mem(ctx, c.out.size_with_stride);
	if ((ret = rknn_set_io_mem(ctx, c.min, &c.in))) die("rknn_set_io_mem in", ret);
	if ((ret = rknn_set_io_mem(ctx, c.mout, &c.out))) die("rknn_set_io_mem out", ret);

	int in_rows = c.s2d || c.nv12 ? SH / 2 : SH, out_rows = c.out.dims[2];
	int uv_n = c.nv12 ? 0 : DH / 2;
	FILE *log = logpath ? fopen(logpath, "w") : NULL;
	if (log) fprintf(log, "t_s,pre,npu,npu_drv,post,uv,e2e\n");

	int cap = sustain > 0 ? (int)(sustain * 100) + 100 : iters;
	double *tp = calloc(cap, 8), *tn = calloc(cap, 8), *td = calloc(cap, 8),
	       *tq = calloc(cap, 8), *tu = calloc(cap, 8), *te = calloc(cap, 8);
	double t_start = now_ms();
	int n = 0;
	for (int i = 0; ; i++) {
		c.src = frames + (size_t)(i % nframes) * SW * SH * 3 / 2;
		double t0 = now_ms();
		par(&c, pre_rows, in_rows);
		rknn_mem_sync(ctx, c.min, RKNN_MEMORY_SYNC_TO_DEVICE);
		double t1 = now_ms();
		if ((ret = rknn_run(ctx, NULL))) die("rknn_run", ret);
		double t2 = now_ms();
		rknn_mem_sync(ctx, c.mout, RKNN_MEMORY_SYNC_FROM_DEVICE);
		par(&c, post_rows, out_rows);
		double t3 = now_ms();
		if (uv_n)
			par(&c, uv_rows, uv_n);
		double t4 = now_ms();
		rknn_perf_run pr = { 0 };
		rknn_query(ctx, RKNN_QUERY_PERF_RUN, &pr, sizeof(pr));
		if (i < warm)
			continue;
		if (n < cap) {
			tp[n] = t1 - t0; tn[n] = t2 - t1; td[n] = pr.run_duration / 1e3;
			tq[n] = t3 - t2; tu[n] = t4 - t3; te[n] = t4 - t0;
			if (log)
				fprintf(log, "%.3f,%.3f,%.3f,%.3f,%.3f,%.3f,%.3f\n", (t4 - t_start) / 1e3,
					tp[n], tn[n], td[n], tq[n], tu[n], te[n]);
			n++;
		}
		if (sustain > 0 ? (t4 - t_start) / 1e3 >= sustain || n >= cap : n >= iters)
			break;
	}
	if (log) fclose(log);
	if (detail) {
		rknn_perf_detail pd;
		if (!rknn_query(ctx, RKNN_QUERY_PERF_DETAIL, &pd, sizeof(pd)))
			printf("%s\n", pd.perf_data);
	}
	printf("model %s variant %s core_mask %d threads %d frames %d (warm-up %d excluded)\n",
	       model, c.nv12 ? "nv12" : c.s2d ? "s2d" : "plain", mask, threads, n, warm);
	stats("pre", tp, n);
	stats("npu", tn, n);
	stats("drv", td, n);
	stats("post", tq, n);
	stats("uv", tu, n);
	stats("e2e", te, n);

	if (outpath) {
		FILE *o = fopen(outpath, "wb");
		for (int k = 0; k < nframes; k++) {
			c.src = frames + (size_t)k * SW * SH * 3 / 2;
			par(&c, pre_rows, in_rows);
			rknn_mem_sync(ctx, c.min, RKNN_MEMORY_SYNC_TO_DEVICE);
			rknn_run(ctx, NULL);
			rknn_mem_sync(ctx, c.mout, RKNN_MEMORY_SYNC_FROM_DEVICE);
			par(&c, post_rows, out_rows);
			if (uv_n)
				par(&c, uv_rows, uv_n);
			fwrite(c.dst, 1, (size_t)DW * DH * 3 / 2, o);
		}
		fclose(o);
		printf("wrote %d frames to %s\n", nframes, outpath);
	}
	rknn_destroy_mem(ctx, c.min);
	rknn_destroy_mem(ctx, c.mout);
	rknn_destroy(ctx);
	return 0;
}
