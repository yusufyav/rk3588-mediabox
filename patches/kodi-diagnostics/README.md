# Kodi patches that only measure

This directory is the diagnostic patch profile. It is applied on top of
`patches/kodi/` only when asked for:

```sh
KODI_PATCH_PROFILE=diagnostic ./scripts/build-kodi.sh
```

**It is empty today, and that is a finding rather than an oversight.**

The question this profile exists to answer is "which of these patches does the
appliance actually need, and which are instrumentation somebody left running?"
Both instrumentation patches were audited against that question and both turned
out to be load-bearing — for different reasons, and neither reason is visible
from reading the patch.

### `0004-instrument-display-colour-state-transitions`

Behaviourally it is nothing but `CLog` calls. Structurally the production series
is anchored on the text it inserts: `0006` adds its functions immediately after
the `LogColorState()` that `0004` introduces, and `0008` needs the include
`0004` adds. Removing it means regenerating the colour-and-EOTF series against a
different base — a change to the path the HDR baseline was measured on.

### `0005-alsa-instrument-sink-initialisation-and-first-write`

It was moved here, and moved back. The patches *apply* cleanly without it — the
production series is 8 patches and 586 insertions on its own — but the build
then fails:

```
AESinkALSA.cpp: In member function 'void CAESinkALSA::HandleError(const char*, int)':
AESinkALSA.cpp:1003:25: error: 'MP2PcmStateName' was not declared in this scope
```

`0007` is production-required: without it, a modeset to a film's own 23.976
mode leaves the HDMI PCM in SETUP and every later write returns `EBADFD`, so
audio is dead for the whole of playback while video carries on. And `0007`
reports what state it recovered from by calling `MP2PcmStateName()`, which
`0005` defines. An instrumentation patch that production code calls into is not
instrumentation any more.

Separating them means moving that helper into `0007` and taking it out of
`0005`, which is patch-series surgery on an audio path that a gate closed. It is
its own piece of work, with its own evidence.

---

The mechanism stays because the classification is the point: every patch in
`patches/kodi/` is there for a reason written down in
[`docs/platform/custom-runtime.md`](../../docs/platform/custom-runtime.md), with
the measured consequence of removing it, and two of them are there for reasons
that are about the shape of the series rather than about the appliance. When
either is resolved, its patch moves here.
