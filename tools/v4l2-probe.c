/*
 * What Chromium asks a V4L2 decoder before it will use it, asked by hand.
 *
 * Chromium's GetSupportedV4L2DecoderConfigs() opens /dev/video0..255, and on
 * each one that opens it runs VIDIOC_QUERYCAP, enumerates the coded formats on
 * the OUTPUT queue, asks VIDIOC_ENUM_FRAMESIZES for each, and turns each codec
 * into a profile list by reading that codec's profile control. If any of those
 * answers is missing the browser lists no decoder and the page falls to the
 * CPU, with nothing in the log to say which question went unanswered.
 *
 * So this asks them in the same order and prints what came back. It is the
 * difference between "the browser is not using the decoder" and knowing which
 * of five questions the decoder got wrong.
 *
 * Built and run by scripts/build-browser-runtime.sh, against the same
 * LD_PRELOAD the browser uses. Give it the node to open; it defaults to
 * /dev/video0, which is where the browser's unit binds the codec list.
 *
 *   cc -O2 -o v4l2-probe tools/v4l2-probe.c
 *   LD_PRELOAD=.../v4l2convert.so ./v4l2-probe /dev/video0
 */
#include <stdio.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <sys/ioctl.h>
#include <linux/videodev2.h>

#ifndef V4L2_CID_MPEG_VIDEO_AV1_PROFILE
#define V4L2_CID_MPEG_VIDEO_AV1_PROFILE (V4L2_CID_CODEC_BASE + 655)
#endif

static void fourcc(char *out, unsigned int f)
{
	out[0] = f & 0xff;
	out[1] = (f >> 8) & 0xff;
	out[2] = (f >> 16) & 0xff;
	out[3] = (f >> 24) & 0xff;
	out[4] = 0;
}

static void enumerate(int fd, const char *label, enum v4l2_buf_type type)
{
	for (int i = 0;; i++) {
		struct v4l2_fmtdesc f;

		memset(&f, 0, sizeof(f));
		f.index = i;
		f.type = type;
		if (ioctl(fd, VIDIOC_ENUM_FMT, &f) < 0)
			return;

		char cc[5];
		fourcc(cc, f.pixelformat);
		printf("  %s[%d] %s  %s\n", label, i, cc, f.description);

		struct v4l2_frmsizeenum fs;
		memset(&fs, 0, sizeof(fs));
		fs.pixel_format = f.pixelformat;
		if (ioctl(fd, VIDIOC_ENUM_FRAMESIZES, &fs) == 0 &&
		    fs.type == V4L2_FRMSIZE_TYPE_STEPWISE)
			printf("        %ux%u .. %ux%u\n",
			       fs.stepwise.min_width, fs.stepwise.min_height,
			       fs.stepwise.max_width, fs.stepwise.max_height);
	}
}

int main(int argc, char **argv)
{
	const char *path = argc > 1 ? argv[1] : "/dev/video0";

	/* The flags Chromium opens with, so the device sees the same request. */
	int fd = open(path, O_RDWR | O_NONBLOCK | O_CLOEXEC);
	if (fd < 0) {
		fprintf(stderr, "open %s: %s\n", path, strerror(errno));
		return 1;
	}

	struct v4l2_capability cap;
	memset(&cap, 0, sizeof(cap));
	if (ioctl(fd, VIDIOC_QUERYCAP, &cap) < 0) {
		fprintf(stderr, "VIDIOC_QUERYCAP: %s\n", strerror(errno));
		return 1;
	}
	printf("driver=%s card=%s caps=0x%08x dev_caps=0x%08x\n",
	       cap.driver, cap.card, cap.capabilities, cap.device_caps);

	enumerate(fd, "OUTPUT", V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE);
	enumerate(fd, "CAPTURE", V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE);

	/*
	 * The control that turns the AV1 codec into a profile list. Without it
	 * Chromium enumerates the format and then supports no AV1 profile at
	 * all, which reads on a page as AV1 simply not being offered.
	 */
	struct v4l2_queryctrl qc;
	memset(&qc, 0, sizeof(qc));
	qc.id = V4L2_CID_MPEG_VIDEO_AV1_PROFILE;
	if (ioctl(fd, VIDIOC_QUERYCTRL, &qc) == 0)
		printf("  AV1_PROFILE ctrl: min=%d max=%d default=%d\n",
		       qc.minimum, qc.maximum, qc.default_value);
	else
		printf("  AV1_PROFILE ctrl: %s\n", strerror(errno));

	close(fd);
	return 0;
}
