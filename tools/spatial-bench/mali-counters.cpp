// mali-counters - the GPU's own hardware counters over a stretch of time.
//
//   mali-counters <seconds>      (or until SIGINT/SIGTERM), one JSON object
//
// Arm's libGPUCounters (MIT, kinstr_prfcnt); not linked by the product.
// build: tools/spatial-bench/build-tools.sh
#include <hwcpipe/counter_database.hpp>
#include <hwcpipe/gpu.hpp>
#include <hwcpipe/sampler.hpp>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <ctime>
#include <vector>

static volatile sig_atomic_t stop;
static double now() { timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec * 1e-9; }

int main(int argc, char **argv) {
    double secs = argc > 1 ? atof(argv[1]) : 1e9;
    signal(SIGINT, [](int) { stop = 1; });
    signal(SIGTERM, [](int) { stop = 1; });
    struct C { hwcpipe_counter id; const char *name; double sum = 0; };
    std::vector<C> cs{{MaliGPUActiveCy, "gpu_active_cy"}, {MaliFragQueueActiveCy, "frag_queue_cy"},
        {MaliVertQueueActiveCy, "vert_queue_cy"}, {MaliTilerActiveCy, "tiler_cy"},
        {MaliExtBusRdBy, "ext_read_bytes"}, {MaliExtBusWrBy, "ext_write_bytes"},
        {MaliFragTile, "frag_tiles"}, {MaliFragTileKill, "frag_tiles_killed"},
        {MaliFragRastQd, "frag_rast_quads"}, {MaliFragShadedQd, "frag_shaded_quads"}, {MaliFragFPKKillQd, "fpk_killed_quads"},
        {MaliGeomTotalPrim, "prims"}, {MaliGeomVisiblePrim, "prims_visible"},
        {MaliSCBusFFEExtRdBy, "tile_load_bytes"}};
    hwcpipe::gpu gpu(0);
    if (!gpu) { fprintf(stderr, "no /dev/mali0\n"); return 1; }
    hwcpipe::sampler_config cfg(gpu);
    std::vector<C> on;
    for (auto &c : cs) {
        auto ec = cfg.add_counter(c.id);
        if (ec) fprintf(stderr, "%s: %s\n", c.name, ec.message().c_str()); else on.push_back(c);
    }
    hwcpipe::sampler<> s(cfg);
    if (auto ec = s.start_sampling()) { fprintf(stderr, "start: %s\n", ec.message().c_str()); return 1; }
    if (s.sample_now()) return 1;
    double t0 = now();
    while (!stop && now() - t0 < secs) {
        timespec d{0, 50000000}; nanosleep(&d, nullptr);
        if (s.sample_now()) continue;
        for (auto &c : on) {
            hwcpipe::counter_sample v;
            if (s.get_counter_value(c.id, v)) continue;
            c.sum += v.type == hwcpipe::counter_sample::type::uint64 ? double(v.value.uint64) : v.value.float64;
        }
    }
    double t = now() - t0;
    printf("{\"seconds\":%.3f", t);
    for (auto &c : on) printf(",\"%s\":%.0f", c.name, c.sum);
    printf("}\n");
    if (s.stop_sampling()) return 1;
}
