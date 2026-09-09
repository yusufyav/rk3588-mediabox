// Shared diagnostics for the mediabox probes.
//
// These tools are instruments first: every interesting value is printed with
// its provenance so that a report can be written from the log alone, without
// re-running anything.
#pragma once

#include <cstdint>

namespace mediabox {

// printf to stdout with a newline, flushed. Line-buffered output matters
// because the probes are usually run over ssh with the output redirected.
void logf(const char *fmt, ...) __attribute__((format(printf, 1, 2)));

// Unrecoverable setup error. Prints errno and leaves via _exit so that no
// atexit handler can run while DRM master is still held.
[[noreturn]] void fail(const char *what);

// Copies a whole procfs/sysfs/debugfs file into the log, bracketed by markers.
void dump_file(const char *path, const char *label);

// DRM fourcc -> printable 4 characters plus NUL.
void fourcc_string(uint32_t format, char out[5]);

// CLOCK_MONOTONIC in seconds. The same clock the DRM page-flip event
// timestamps use on this driver, so flip times and deadlines are comparable.
double monotonic_seconds();

// Sleeps until an absolute CLOCK_MONOTONIC deadline, restarting on EINTR.
// Returns immediately if the deadline has already passed.
void sleep_until_monotonic(double deadline_seconds);

}  // namespace mediabox
