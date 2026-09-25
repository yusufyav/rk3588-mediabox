// Kodi's reader of /run/mediabox/output-plan (patches/kodi/0013,
// xbmc/windowing/gbm/OutputPlan.cpp), tested on its own: a plan is used only
// when it was made for the display that is there now.
//
// Built by tests/run-host-tests.sh against the OutputPlan sources taken out of
// the patch itself, so what is tested is what Kodi is built with.

#include "OutputPlan.h"

#include <algorithm>
#include <cstdio>
#include <string>
#include <vector>

using namespace KODI::WINDOWING::GBM::OUTPUTPLAN;

namespace
{
int failures = 0;

void Check(const char* what, bool ok)
{
  std::printf("%s %s\n", ok ? "ok  " : "FAIL", what);
  if (!ok)
    failures++;
}

const std::string BOOT = "06aa84e3-5fff-4784-8545-ca153e679661";
const std::string SONY = "1175a696da42d0dc913a90983653ceef0ba6572ba64f85c1c8826ab43345fc54";
const std::string OTHER = "9f0c4b2a1d7e3a55c0ffee0000000000000000000000000000000000000000aa";
const std::string MODE = "3840x2160@296703/5500x2250";

std::string PlanText(const std::string& boot = BOOT,
                     const std::string& generation = "2",
                     const std::string& connector = "HDMI-A-2",
                     const std::string& edid = SONY)
{
  return "schema=2\nboot_id=" + boot + "\ngeneration=" + generation + "\nconnector=" + connector +
         "\ntransmitter=fdea0000.hdmi\nedid_sha256=" + edid +
         "\ntiming_key=t1:594000:3840,4016,4104,4400,0:2160,2168,2178,2250,0:0005"
         "\nsource_profile=rk3588-vendor61-dw-hdmi-qp@2\nkodi_screenmode=0384002160060.00000pstd"
         "\nkodi_whitelist=0384002160060.00000pstd\nbrowser_mode=3840x2160@60.000Hz\n"
         "colour 3840x2160@594000/4400x2250 rgb:8 none\n"
         "colour " +
         MODE + " rgb:8 ycbcr422:10\n";
}

Display Now()
{
  Display now;
  now.bootId = BOOT;
  now.observed = true;
  now.observerBootId = BOOT;
  now.generation = 2;
  now.connected = true;
  now.observerConnector = "HDMI-A-2";
  now.observerEdidSha256 = SONY;
  now.connector = "HDMI-A-2";
  now.edidSha256 = SONY;
  return now;
}

Decision ReadWith(const std::optional<std::string>& text, const Display& now)
{
  return Read([&] { return text; }, [&] { return now; }, MODE);
}

bool Rejected(const Decision& decision)
{
  return !decision.accepted && !decision.colours && decision.provenance.empty() &&
         !decision.why.empty();
}
} // namespace

int main()
{
  {
    const Decision d = ReadWith(PlanText(), Now());
    Check("current plan -> accepted", d.accepted && d.why.empty());
    Check("  with the colours at its mode",
          d.colours && d.colours->sdr == Colour{"rgb", 8} && d.colours->hdr == Colour{"ycbcr422", 10});
    Check("  and its provenance for the log", d.provenance == BOOT + ":2 HDMI-A-2 1175a696da42");
  }
  {
    const Decision d = Read([] { return std::optional<std::string>(PlanText()); }, Now,
                            "1920x1080@148500/2200x1125");
    Check("current plan with no line for the mode -> accepted, no colours",
          d.accepted && !d.colours);
  }
  {
    Display now = Now();
    now.observerEdidSha256 = OTHER;
    now.edidSha256 = OTHER;
    Check("different EDID SHA -> rejected", Rejected(ReadWith(PlanText(), now)));
  }
  {
    // The sink changed and the observer has not looked yet: its generation and
    // sink still match the plan, the EDID on Kodi's connector does not.
    Display now = Now();
    now.edidSha256 = OTHER;
    Check("different EDID on Kodi's connector before the observer saw it -> rejected",
          Rejected(ReadWith(PlanText(), now)));
  }
  {
    Display now = Now();
    now.edidSha256.clear();
    Check("no EDID on Kodi's connector -> rejected", Rejected(ReadWith(PlanText(), now)));
  }
  {
    Display now = Now();
    now.generation = 4;
    Check("different generation -> rejected", Rejected(ReadWith(PlanText(), now)));
    Check("  a plan made for a later generation too",
          Rejected(ReadWith(PlanText(BOOT, "5"), Now())));
  }
  {
    const std::string other = "11111111-2222-3333-4444-555555555555";
    Check("different boot id -> rejected", Rejected(ReadWith(PlanText(other), Now())));
    Display now = Now();
    now.observerBootId = other;
    Check("  an observer snapshot from another boot too", Rejected(ReadWith(PlanText(), now)));
    now = Now();
    now.bootId.clear();
    Check("  no boot id to check against too", Rejected(ReadWith(PlanText(), now)));
  }
  {
    Check("different connector -> rejected",
          Rejected(ReadWith(PlanText(BOOT, "2", "HDMI-A-1"), Now())));
    Display now = Now();
    now.connector = "HDMI-A-1";
    Check("  Kodi driving another connector than the plan's too",
          Rejected(ReadWith(PlanText(), now)));
    now = Now();
    now.observerConnector = "HDMI-A-1";
    Check("  the observer seeing another connector too", Rejected(ReadWith(PlanText(), now)));
  }
  {
    Check("missing provenance: a plan of colour lines only -> rejected",
          Rejected(ReadWith("colour " + MODE + " rgb:8 ycbcr422:10\n", Now())));
    for (const char* key : {"schema", "boot_id", "generation", "connector", "edid_sha256"})
    {
      std::string text = PlanText();
      const size_t at = text.find(std::string(key) + "=");
      text.erase(at, text.find('\n', at) - at + 1);
      Check((std::string("missing provenance: no ") + key + " -> rejected").c_str(),
            Rejected(ReadWith(text, Now())));
    }
    std::string schema1 = PlanText();
    schema1.replace(0, 8, "schema=1");
    Check("  a plan of another schema -> rejected", Rejected(ReadWith(schema1, Now())));
    Check("  a generation that is not a number -> rejected",
          Rejected(ReadWith(PlanText(BOOT, "2x"), Now())));
  }
  {
    Check("no plan -> rejected", Rejected(ReadWith(std::nullopt, Now())));
    Display now = Now();
    now.observed = false;
    Check("no recent observer snapshot -> rejected", Rejected(ReadWith(PlanText(), now)));
    now = Now();
    now.connected = false;
    Check("the observer sees no sink -> rejected", Rejected(ReadWith(PlanText(), now)));
  }
  {
    // The plan checked is A; by the second look the control plane has
    // replaced it. A was current when it was checked and is not used.
    std::vector<std::string> plans = {PlanText(), PlanText(BOOT, "3")};
    size_t reads = 0;
    Display now = Now();
    const Decision d = Read(
        [&] {
          const std::string text = plans[std::min(reads, plans.size() - 1)];
          reads++;
          return std::optional<std::string>(text);
        },
        [&] { return now; }, MODE);
    Check("plan changes between validation/use -> stale plan not used",
          Rejected(d) && reads == 2);
  }
  {
    // The generation ends while the plan is checked; the file has not been
    // touched yet.
    size_t looks = 0;
    const Decision d = Read([] { return std::optional<std::string>(PlanText()); },
                            [&] {
                              Display now = Now();
                              if (looks++ > 0)
                                now.generation = 3;
                              return now;
                            },
                            MODE);
    Check("display changes between validation/use -> stale plan not used",
          Rejected(d) && looks == 2);
  }
  {
    // And the other way: a sink that goes and comes back between the looks is
    // a different display state for the instant it was gone.
    size_t looks = 0;
    const Decision d = Read([] { return std::optional<std::string>(PlanText()); },
                            [&] {
                              Display now = Now();
                              if (looks++ == 1)
                                now.edidSha256 = OTHER;
                              return now;
                            },
                            MODE);
    Check("EDID changes between validation/use -> stale plan not used", Rejected(d));
  }
  {
    std::string why;
    const std::optional<Plan> plan = Parse(PlanText(), why);
    Check("a colour of none is no colour",
          plan && ColoursAt(*plan, "3840x2160@594000/4400x2250") &&
              !ColoursAt(*plan, "3840x2160@594000/4400x2250")->hdr);
  }

  std::printf("%d failure(s)\n", failures);
  return failures == 0 ? 0 : 1;
}
