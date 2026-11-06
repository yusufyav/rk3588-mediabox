# mediabox-dvb test fixtures

Transport streams for `service::tests::one_tsid_on_two_networks_only_the_channel_s_opens_from_its_own_pat`:
one transport stream ID on two networks, the receiver's admission gate and
writer fed what a retune gives them. Made with FFmpeg's MPEG-TS muxer (PAT,
PMT and SDT actual with their CRCs; one second of 64x64 H.264), PMT PID 4096
and video PID 256 in all of them, a PAT every 0.1 s and an SDT every 0.25 s:

| file | TSID | ONID | SID | pictures |
| --- | --- | --- | --- | --- |
| `same-tsid-old.ts` | 7 | 1 | 101 | `testsrc` |
| `same-tsid-new.ts` | 7 | 318 | 202 | `testsrc2` |
| `same-tsid-other-network.ts` | 7 | 999 | 202 | `testsrc2` |
| `same-tsid-twin.ts` | 7 | 318 | 101 | `testsrc2` |

```sh
mk() { ffmpeg -f lavfi -i "$5=s=64x64:r=25:d=1" -c:v libx264 -preset veryfast -g 25 -bf 0 -b:v 30k \
  -f mpegts -mpegts_transport_stream_id 7 -mpegts_original_network_id $1 -mpegts_service_id $2 \
  -mpegts_pmt_start_pid 4096 -mpegts_start_pid 256 -metadata service_name=$3 \
  -metadata service_provider=MediaBox -pat_period 0.1 -sdt_period 0.25 "$4"; }
mk 1 101 OLD same-tsid-old.ts testsrc
mk 318 202 NEW same-tsid-new.ts testsrc2
mk 999 202 NEW same-tsid-other-network.ts testsrc2
mk 318 101 TWIN same-tsid-twin.ts testsrc2
```

The old stream's pictures differ from the others', so none of its video
packets is byte for byte one of theirs: one in the output is its tail.
