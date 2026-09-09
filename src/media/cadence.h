// Frame pacing and cadence measurement.
//
// The instrument here is the DRM page-flip event's vblank sequence number.
// Wall-clock intervals drift and can be blamed on scheduling; the vblank
// counter cannot. A sequence delta of one between consecutive flips means the
// previous frame occupied exactly one refresh interval, so:
//
//     repeated frames = sum over flips of (sequence_delta - 1)
//
// which is the number a judder complaint actually reduces to. Intervals are
// still recorded, from the flip timestamps rather than from CPU time, because
// percentiles over them are what makes "acceptable cadence" checkable.
#pragma once

#include <cstdint>
#include <vector>

namespace mediabox::media {

struct CadenceStats {
    long decoded = 0;
    long presented = 0;   // successful page flips
    long dropped = 0;     // decoded but deliberately not presented, to catch up
    long repeated = 0;    // extra refresh intervals a frame stayed on screen
    long late = 0;        // flips that landed more than half a refresh past target
    long flip_errors = 0; // atomic commit failures on the flip path
    long flip_timeouts = 0;
    long no_pts = 0;      // frames with no usable PTS; presented, not scheduled

    // Frames shown before the playback clock was anchored. Startup -- the
    // seek, the decoder filling its buffer pool, the USB disk spinning up its
    // read-ahead -- is not a cadence result, so it is counted separately
    // rather than allowed to contaminate the steady-state percentiles.
    long warmup_presented = 0;
    long warmup_repeated = 0;
    long warmup_dropped = 0;

    double first_flip_time = 0.0;
    double last_flip_time = 0.0;
    unsigned int first_sequence = 0;
    unsigned int last_sequence = 0;

    std::vector<double> intervals_ms;    // between consecutive flips
    std::vector<double> lateness_ms;     // flip time minus scheduled target
};

// Percentile over an unsorted sample, using nearest-rank on a sorted copy.
double percentile(std::vector<double> values, double p);

double mean(const std::vector<double> &values);

// Paces presentation against stream PTS.
//
// The playback clock is anchored on the first presented frame, so a seek into
// the middle of a film is free: only PTS *deltas* matter. A frame that is due
// more than `drop_after` refresh intervals in the past is dropped rather than
// presented, because presenting it would only push everything after it further
// behind.
class Pacer {
  public:
    Pacer(double refresh_hz, double drop_after_intervals)
        : vblank_(refresh_hz > 0.0 ? 1.0 / refresh_hz : 0.0),
          drop_after_(drop_after_intervals) {}

    double vblank_seconds() const { return vblank_; }

    // Establishes the anchor. Call once, with the PTS of the first frame that
    // will actually be shown, at the moment it is shown.
    void anchor(double pts_seconds, double now);
    bool anchored() const { return anchored_; }

    // Where this frame should appear, in CLOCK_MONOTONIC seconds.
    double target_for(double pts_seconds) const;

    // True if the frame is so late that presenting it is pointless.
    bool should_drop(double target, double now) const;

    // Sleeps until just before the target vblank. Returns without sleeping if
    // the target is already at hand, which is the normal case when content
    // rate and display rate match.
    void wait_for(double target) const;

  private:
    double vblank_ = 0.0;
    double drop_after_ = 0.0;
    bool anchored_ = false;
    double anchor_pts_ = 0.0;
    double anchor_time_ = 0.0;
};

// Splits startup off from steady state: the counts so far become the warmup
// figures and measurement restarts from the next flip.
void start_steady_state(CadenceStats *stats);

// Folds one completed flip into the statistics.
void record_flip(CadenceStats *stats, unsigned int sequence, double timestamp, double target,
                 double vblank_seconds);

void log_cadence(const CadenceStats &stats, double expected_period_ms);

}  // namespace mediabox::media
