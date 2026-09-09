#include "media/cadence.h"

#include "common/log.h"

#include <algorithm>
#include <cmath>

namespace mediabox::media {

double percentile(std::vector<double> values, double p) {
    if (values.empty()) return 0.0;
    std::sort(values.begin(), values.end());
    if (p <= 0.0) return values.front();
    if (p >= 1.0) return values.back();
    const size_t rank = static_cast<size_t>(std::ceil(p * static_cast<double>(values.size())));
    const size_t index = rank ? rank - 1 : 0;
    return values[std::min(index, values.size() - 1)];
}

double mean(const std::vector<double> &values) {
    if (values.empty()) return 0.0;
    double sum = 0.0;
    for (double v : values) sum += v;
    return sum / static_cast<double>(values.size());
}

void Pacer::anchor(double pts_seconds, double now) {
    anchor_pts_ = pts_seconds;
    anchor_time_ = now;
    anchored_ = true;
}

double Pacer::target_for(double pts_seconds) const {
    if (!anchored_) return 0.0;
    return anchor_time_ + (pts_seconds - anchor_pts_);
}

bool Pacer::should_drop(double target, double now) const {
    if (!anchored_ || vblank_ <= 0.0) return false;
    return (now - target) > drop_after_ * vblank_;
}

void Pacer::wait_for(double target) const {
    if (!anchored_ || vblank_ <= 0.0) return;
    // Commit half a refresh interval early: the atomic commit is latched at
    // the next vblank, so aiming exactly at the target would be a coin flip
    // between the intended vblank and the one after it.
    const double deadline = target - 0.5 * vblank_;
    const double now = mediabox::monotonic_seconds();
    if (deadline <= now) return;
    mediabox::sleep_until_monotonic(deadline);
}

void start_steady_state(CadenceStats *stats) {
    stats->warmup_presented = stats->presented;
    stats->warmup_repeated = stats->repeated;
    stats->warmup_dropped = stats->dropped;
    stats->presented = 0;
    stats->repeated = 0;
    stats->dropped = 0;
    stats->late = 0;
    stats->intervals_ms.clear();
    stats->lateness_ms.clear();
}

void record_flip(CadenceStats *stats, unsigned int sequence, double timestamp, double target,
                 double vblank_seconds) {
    if (stats->presented == 0) {
        stats->first_flip_time = timestamp;
        stats->first_sequence = sequence;
    } else {
        stats->intervals_ms.push_back((timestamp - stats->last_flip_time) * 1000.0);
        // Wraparound is not special-cased: the counter is 32-bit and this gate
        // runs for minutes, so a wrap would need the link up for years.
        const long delta = static_cast<long>(sequence) - static_cast<long>(stats->last_sequence);
        if (delta > 1) stats->repeated += delta - 1;
    }
    stats->last_flip_time = timestamp;
    stats->last_sequence = sequence;
    ++stats->presented;

    if (target > 0.0) {
        const double lateness_ms = (timestamp - target) * 1000.0;
        stats->lateness_ms.push_back(lateness_ms);
        if (vblank_seconds > 0.0 && (timestamp - target) > 0.5 * vblank_seconds) ++stats->late;
    }
}

void log_cadence(const CadenceStats &s, double expected_period_ms) {
    const double span = s.last_flip_time - s.first_flip_time;
    mediabox::logf("cadence warmup presented=%ld repeated=%ld dropped=%ld (excluded from the "
                   "steady-state figures below)",
                   s.warmup_presented, s.warmup_repeated, s.warmup_dropped);
    mediabox::logf("cadence decoded=%ld presented=%ld dropped=%ld repeated=%ld late=%ld "
                   "flip_errors=%ld flip_timeouts=%ld no_pts=%ld",
                   s.decoded, s.presented, s.dropped, s.repeated, s.late, s.flip_errors,
                   s.flip_timeouts, s.no_pts);
    mediabox::logf("cadence totals presented=%ld repeated=%ld dropped=%ld (warmup + steady state)",
                   s.warmup_presented + s.presented, s.warmup_repeated + s.repeated,
                   s.warmup_dropped + s.dropped);
    mediabox::logf("cadence vblank_sequence first=%u last=%u delta=%ld presented=%ld "
                   "(delta-presented+1=%ld extra refresh intervals)",
                   s.first_sequence, s.last_sequence,
                   static_cast<long>(s.last_sequence) - static_cast<long>(s.first_sequence),
                   s.presented,
                   static_cast<long>(s.last_sequence) - static_cast<long>(s.first_sequence) -
                       s.presented + 1);
    if (span > 0.0)
        mediabox::logf("cadence wall_span=%.3fs effective_rate=%.4f fps (expected %.4f fps)", span,
                       static_cast<double>(s.presented - 1) / span,
                       expected_period_ms > 0.0 ? 1000.0 / expected_period_ms : 0.0);
    if (!s.intervals_ms.empty())
        mediabox::logf("cadence frame_interval_ms expected=%.3f mean=%.3f p50=%.3f p95=%.3f "
                       "p99=%.3f min=%.3f max=%.3f n=%zu",
                       expected_period_ms, mean(s.intervals_ms), percentile(s.intervals_ms, 0.50),
                       percentile(s.intervals_ms, 0.95), percentile(s.intervals_ms, 0.99),
                       percentile(s.intervals_ms, 0.0), percentile(s.intervals_ms, 1.0),
                       s.intervals_ms.size());
    if (!s.lateness_ms.empty())
        mediabox::logf("cadence lateness_ms mean=%.3f p50=%.3f p95=%.3f p99=%.3f min=%.3f max=%.3f",
                       mean(s.lateness_ms), percentile(s.lateness_ms, 0.50),
                       percentile(s.lateness_ms, 0.95), percentile(s.lateness_ms, 0.99),
                       percentile(s.lateness_ms, 0.0), percentile(s.lateness_ms, 1.0));
}

}  // namespace mediabox::media
