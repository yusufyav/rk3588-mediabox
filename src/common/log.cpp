#include "common/log.h"

#include <cerrno>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <unistd.h>

namespace mediabox {

void logf(const char *fmt, ...) {
    va_list args;
    va_start(args, fmt);
    vprintf(fmt, args);
    va_end(args);
    fputc('\n', stdout);
    fflush(stdout);
}

void fail(const char *what) {
    fprintf(stderr, "error: %s: %s\n", what, strerror(errno));
    fflush(nullptr);
    _exit(EXIT_FAILURE);
}

void dump_file(const char *path, const char *label) {
    FILE *f = fopen(path, "r");
    if (!f) {
        logf("%s unavailable (%s)", label, strerror(errno));
        return;
    }
    logf("---- %s begin (%s) ----", label, path);
    char line[1024];
    while (fgets(line, sizeof(line), f)) fputs(line, stdout);
    logf("---- %s end ----", label);
    fclose(f);
    fflush(stdout);
}

void fourcc_string(uint32_t format, char out[5]) {
    out[0] = static_cast<char>(format & 0xff);
    out[1] = static_cast<char>((format >> 8) & 0xff);
    out[2] = static_cast<char>((format >> 16) & 0xff);
    out[3] = static_cast<char>((format >> 24) & 0xff);
    out[4] = '\0';
}

double monotonic_seconds() {
    struct timespec ts {};
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return static_cast<double>(ts.tv_sec) + static_cast<double>(ts.tv_nsec) / 1e9;
}

void sleep_until_monotonic(double deadline_seconds) {
    if (deadline_seconds <= 0.0) return;
    struct timespec ts {};
    ts.tv_sec = static_cast<time_t>(deadline_seconds);
    ts.tv_nsec = static_cast<long>((deadline_seconds - static_cast<double>(ts.tv_sec)) * 1e9);
    if (ts.tv_nsec < 0) ts.tv_nsec = 0;
    if (ts.tv_nsec > 999999999L) ts.tv_nsec = 999999999L;
    while (clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &ts, nullptr) == EINTR) {
    }
}

}  // namespace mediabox
