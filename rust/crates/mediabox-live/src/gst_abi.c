/* The audio sink's and its clock's instance fields the sound clock's
 * controller reads (audio_clock.rs), compiled against the GStreamer
 * headers the player is built with: the compiler resolves the layout,
 * nobody writes an offset. What GStreamer gives as API is taken through
 * it; GstAudioBaseSink's next_sample and provided_clock are public
 * instance fields with no getter (gstaudiobasesink.h), so they are read
 * here, and only they. The run-time guard (gst_abi.rs) holds these
 * headers to the libraries loaded. */
#include <gst/audio/audio.h>

/* The headers' GStreamer version, major * 1e6 + minor * 1e3 + micro. */
guint
mbx_gst_headers_version (void)
{
  return GST_VERSION_MAJOR * 1000000u + GST_VERSION_MINOR * 1000u + GST_VERSION_MICRO;
}

GType
mbx_audio_base_sink_type (void)
{
  return GST_TYPE_AUDIO_BASE_SINK;
}

GType
mbx_audio_clock_type (void)
{
  return GST_TYPE_AUDIO_CLOCK;
}

gsize
mbx_audio_base_sink_size (void)
{
  return sizeof (GstAudioBaseSink);
}

gsize
mbx_audio_clock_size (void)
{
  return sizeof (GstAudioClock);
}

/* The sample the sink writes the next buffer at; -1 when it will place
 * the buffer at its time. Read on the streaming thread that writes it. */
guint64
mbx_audio_base_sink_next_sample (GstElement * sink)
{
  return ((GstAudioBaseSink *) sink)->next_sample;
}

/* The clock the sink made at its init, held to its finalize (no ref). */
GstClock *
mbx_audio_base_sink_clock (GstElement * sink)
{
  return ((GstAudioBaseSink *) sink)->provided_clock;
}

/* The audio clock's user data: the sink that made it. */
gpointer
mbx_audio_clock_user_data (GstClock * clock)
{
  return ((GstAudioClock *) clock)->user_data;
}

/* The clock's offset, through its API: gst_audio_clock_adjust adds it. */
GstClockTimeDiff
mbx_audio_clock_time_offset (GstClock * clock)
{
  return (GstClockTimeDiff) gst_audio_clock_adjust ((GstAudioClock *) clock, 0);
}
