"""Subtitle files in and out: the times change, nothing else does."""

from __future__ import annotations

import gzip
import unittest

from media.subtitles.formats import SubtitleFormatError, parse, parse_text


SRT = """1
00:00:01,000 --> 00:00:03,500
Merhaba.

2
00:00:05,250 --> 00:00:07,000
<i>İki satır,</i>
ikinci satır.

3
00:01:00,000 --> 00:01:02,000
Üç.
"""

VTT = """WEBVTT

intro
00:01.000 --> 00:03.500 align:start position:10%
Hello

00:00:05.250 --> 00:00:07.000
World
"""

ASS = """[Script Info]
ScriptType: v4.00+
PlayResX: 1920

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour
Style: Default,Arial,48,&H00FFFFFF

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:03.50,Default,,0,0,0,,{\\pos(960,1000)}Merhaba, dünya
Comment: 0,0:00:02.00,0:00:04.00,Default,,0,0,0,,not a line
Dialogue: 0,0:00:05.25,0:00:07.00,Default,,0,0,0,,{\\an8}İkinci
"""


class Reading(unittest.TestCase):
    def test_srt(self):
        document = parse_text(SRT)
        self.assertEqual(document.format, "srt")
        self.assertEqual([(c.start, c.end) for c in document.cues], [(1.0, 3.5), (5.25, 7.0), (60.0, 62.0)])

    def test_webvtt_with_and_without_hours(self):
        document = parse_text(VTT)
        self.assertEqual(document.format, "vtt")
        self.assertEqual([(c.start, c.end) for c in document.cues], [(1.0, 3.5), (5.25, 7.0)])

    def test_ass_reads_dialogue_not_comments(self):
        document = parse_text(ASS)
        self.assertEqual(document.format, "ass")
        self.assertEqual([(c.start, c.end) for c in document.cues], [(1.0, 3.5), (5.25, 7.0)])

    def test_ssa_is_told_apart(self):
        document = parse_text(ASS.replace("v4.00+", "v4.00"))
        self.assertEqual(document.format, "ssa")

    def test_a_turkish_subtitle_in_its_old_code_page(self):
        raw = SRT.encode("cp1254")
        document = parse(raw, "tur")
        self.assertIn("İki satır,", document.text())
        self.assertIn("Üç.", document.text())

    def test_bom_and_gzip(self):
        raw = gzip.compress(b"\xef\xbb\xbf" + SRT.encode("utf-8"))
        self.assertEqual(len(parse(raw).cues), 3)

    def test_crlf(self):
        self.assertEqual(len(parse_text(SRT.replace("\n", "\r\n")).cues), 3)


class Malformed(unittest.TestCase):
    def test_not_a_subtitle(self):
        for text in ("", "hello world", "<html><body>404</body></html>", "{1}{50}MicroDVD line"):
            with self.subTest(text=text):
                with self.assertRaises(SubtitleFormatError):
                    parse_text(text)

    def test_times_that_run_backwards_are_skipped(self):
        document = parse_text("1\n00:00:05,000 --> 00:00:04,000\nbad\n\n2\n00:00:06,000 --> 00:00:07,000\ngood\n")
        self.assertEqual(len(document.cues), 1)

    def test_an_ass_file_without_events(self):
        with self.assertRaises(SubtitleFormatError):
            parse_text("[Script Info]\nScriptType: v4.00+\n")

    def test_a_broken_gzip(self):
        with self.assertRaises(SubtitleFormatError):
            parse(b"\x1f\x8b\x08\x00garbage")


class Writing(unittest.TestCase):
    def test_srt_times_move_and_text_stays(self):
        document = parse_text(SRT).remap(lambda s, e: (s * 1.5 + 2, e * 1.5 + 2))
        self.assertEqual([(c.start, c.end) for c in document.cues], [(3.5, 7.25), (9.875, 12.5), (92.0, 95.0)])
        text = document.text()
        self.assertIn("00:00:03,500 --> 00:00:07,250", text)
        self.assertIn("<i>İki satır,</i>\nikinci satır.", text)

    def test_vtt_settings_survive(self):
        text = parse_text(VTT).remap(lambda s, e: (s + 1, e + 1)).text()
        self.assertIn("00:00:02.000 --> 00:00:04.500 align:start position:10%", text)
        self.assertTrue(text.startswith("WEBVTT"))

    def test_ass_styles_and_tags_survive(self):
        document = parse_text(ASS).remap(lambda s, e: (s + 10, e + 10))
        text = document.text()
        self.assertIn("Style: Default,Arial,48,&H00FFFFFF", text)
        self.assertIn("Dialogue: 0,0:00:11.00,0:00:13.50,Default,,0,0,0,,{\\pos(960,1000)}Merhaba, dünya", text)
        self.assertIn("Comment: 0,0:00:02.00,0:00:04.00", text)
        self.assertEqual(parse_text(text).cues[1].start, 15.25)

    def test_dropped_lines_leave_no_orphans(self):
        document = parse_text(SRT).remap(lambda s, e: None if s == 5.25 else (s, e))
        self.assertEqual(len(document.cues), 2)
        self.assertNotIn("ikinci satır", document.text())
        self.assertNotIn("\n2\n", document.text())
        ass = parse_text(ASS).remap(lambda s, e: None if s == 1.0 else (s, e))
        self.assertNotIn("Merhaba", ass.text())
        self.assertIn("İkinci", ass.text())

    def test_the_original_is_not_touched(self):
        original = parse_text(SRT)
        before = original.text()
        original.remap(lambda s, e: (s + 5, e + 5))
        self.assertEqual(original.text(), before)


if __name__ == "__main__":
    unittest.main()
