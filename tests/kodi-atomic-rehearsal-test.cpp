// Which of Kodi's atomic commits are rehearsed with TEST_ONLY first
// (patches/kodi/0014, xbmc/windowing/gbm/drm/AtomicRehearsal.h), tested on its
// own: a commit that changes the connector's colour goes without a rehearsal,
// because the vendor HDMI driver's check is what leaves the AVI infoframe
// stale; every other commit keeps it.
//
// Built by tests/run-host-tests.sh against the header taken out of the patch
// itself, so what is tested is what Kodi is built with.

#include "AtomicRehearsal.h"

#include <cstdio>
#include <string>
#include <vector>

using namespace KODI::WINDOWING::GBM::ATOMIC;

namespace
{
int failures = 0;

void Check(const char* what, bool ok)
{
  std::printf("%s %s\n", ok ? "ok  " : "FAIL", what);
  if (!ok)
    failures++;
}

// The connector's share of a request, in the order CDRMAtomic::DrmAtomicCommit
// builds it: the caller's properties first, then -- for a modeset -- the
// CRTC_ID the commit adds itself. The decision is taken between the two, which
// is where DrmAtomicCommit takes it.
struct Commit
{
  bool rehearsed;
  std::vector<std::string> connector;
};

Commit DrmAtomicCommit(std::vector<std::string> connector, bool modeset)
{
  const bool rehearse = RehearseWithTestOnly(connector);
  if (modeset)
    connector.emplace_back("CRTC_ID");
  return {rehearse, connector};
}

bool Carries(const Commit& commit, const std::string& name)
{
  for (const auto& property : commit.connector)
    if (property == name)
      return true;
  return false;
}
} // namespace

int main()
{
  // 1. A page flip touches planes only.
  Check("a page flip is rehearsed", DrmAtomicCommit({}, false).rehearsed);

  // 2 and 3. A modeset alone: the connector's CRTC_ID is in the request that
  // is committed, and it is not a reason to skip the rehearsal.
  const Commit modeset = DrmAtomicCommit({}, true);
  Check("a modeset alone is rehearsed", modeset.rehearsed);
  Check("and its request does carry the connector's CRTC_ID", Carries(modeset, "CRTC_ID"));
  Check("a CRTC_ID on the connector is not a colour change",
        RehearseWithTestOnly(std::vector<std::string>{"CRTC_ID"}));

  // 4 to 7. Each colour property on its own.
  Check("a color_format change is not rehearsed",
        !DrmAtomicCommit({"color_format"}, false).rehearsed);
  Check("a Colorspace change is not rehearsed",
        !DrmAtomicCommit({"Colorspace"}, false).rehearsed);
  Check("an HDR_OUTPUT_METADATA change is not rehearsed",
        !DrmAtomicCommit({"HDR_OUTPUT_METADATA"}, false).rehearsed);
  Check("a color_depth change is not rehearsed",
        !DrmAtomicCommit({"color_depth"}, false).rehearsed);
  Check("a max bpc change is not rehearsed", !DrmAtomicCommit({"max bpc"}, false).rehearsed);

  // 8. A colour change that comes with a modeset, as a refresh-rate switch
  // into HDR does.
  const Commit both = DrmAtomicCommit({"Colorspace", "HDR_OUTPUT_METADATA", "color_format"}, true);
  Check("a colour change with a modeset is not rehearsed", !both.rehearsed);
  Check("and still commits the modeset", Carries(both, "CRTC_ID"));

  // The list is by name and nothing wider: a connector property the output
  // colour is not worked out from keeps the rehearsal.
  for (const char* other : {"DPMS", "Content Protection", "link-status", "allm_enable",
                            "quant_range", "output_hdmi_dvi", "color_format_caps"})
  {
    const std::string what = std::string("a ") + other + " change is rehearsed";
    Check(what.c_str(), DrmAtomicCommit({other}, false).rehearsed);
  }
  Check("names are matched exactly", DrmAtomicCommit({"colorspace", "max_bpc"}, false).rehearsed);

  return failures == 0 ? 0 : 1;
}
