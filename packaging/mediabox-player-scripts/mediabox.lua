-- The bridge between a key press in the player and the control plane.
--
-- The player does not know what Kodi is, and should not: it asks the daemon to
-- hand the film over, and the daemon is the one place that knows how to create
-- the session, take the display and open it there.

local function handoff()
    mp.osd_message("Kodi'ye aktarılıyor…", 3)
    -- One request, and then this player is over: the daemon stops it as part
    -- of the handover, so there is nothing to do here afterwards.
    mp.command_native_async({
        name = "subprocess",
        playback_only = false,
        args = {
            "/usr/bin/curl", "-s", "-m", "120",
            "-H", "content-type: application/json",
            "-d", '{"command":"media_handoff_to_kodi"}',
            "http://127.0.0.1:8787/v1/control",
        },
    }, function() end)
end

mp.register_script_message("handoff", handoff)
