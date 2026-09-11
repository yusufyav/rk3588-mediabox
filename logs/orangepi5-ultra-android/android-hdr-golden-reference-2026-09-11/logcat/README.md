# logcat

`hwc-verbose-transitions.log` — `logcat -d -v threadtime` covering one
G1→G2→G3→G4→G1 pass driven by input key events inside the same Kodi session.

## Property handling

`vendor.hwc.log` was **empty (unset)** before this capture. The HWC reads it
through `android::hwc_get_int_property()` into `android::g_log_level`, both
present in the running binary, so the level is read at runtime and no service
restart was needed.

    original value : <empty>
    set to         : 255   (all LOG_LEVEL bits)
    restored to    : ""    -> getprop returns empty again

Android has no API to delete a property, so the restore sets an empty string,
which reproduces the original observable semantics exactly: `getprop
vendor.hwc.log` returns empty and `hwc_get_int_property` falls back to its
default. The composer service was not restarted at any point.

## What the log shows

Every committed frame carries both planes with their EOTF explicitly programmed:

    plane=Cluster0-win0 ... zpos=0 ... eotf=2 colorspace=a   <- HDR video
    plane=Esmart0-win0  ... zpos=1 ... eotf=0 colorspace=0   <- SDR GUI

and the layer dump classifies them the same way (`hdr=1` for the YU10 video
layer, `hdr=0` for the AB24 GUI layer). `hwc-drm-utils: GetEOTF has st2084`
appears once per frame: the HWC derives PQ from the video layer's dataspace
only, and never propagates it to the GUI layer.
