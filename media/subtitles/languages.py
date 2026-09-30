"""Language codes as subtitle addons write them, made comparable.

Addons send ISO 639-2 (`tur`, `eng`, both `ger` and `deu`), ISO 639-1 (`tr`),
OpenSubtitles' own (`pob` for Brazilian Portuguese) and sometimes a name.
Embedded tracks carry whatever the muxer wrote, usually 639-2. Preference and
matching work on one canonical form: the two-letter code where there is one.
"""

from __future__ import annotations

_CANONICAL = {
    "tur": "tr", "eng": "en", "ger": "de", "deu": "de", "fre": "fr", "fra": "fr",
    "spa": "es", "spn": "es", "ita": "it", "por": "pt", "pob": "pt-br", "pb": "pt-br", "ptb": "pt-br",
    "rus": "ru", "ara": "ar", "jpn": "ja", "kor": "ko", "chi": "zh", "zho": "zh",
    "zht": "zh", "zhe": "zh", "dut": "nl", "nld": "nl", "pol": "pl", "swe": "sv",
    "nor": "no", "nob": "no", "dan": "da", "fin": "fi", "gre": "el", "ell": "el",
    "heb": "he", "hun": "hu", "cze": "cs", "ces": "cs", "rum": "ro", "ron": "ro",
    "bul": "bg", "hrv": "hr", "scr": "hr", "srp": "sr", "scc": "sr", "ukr": "uk",
    "per": "fa", "fas": "fa", "hin": "hi", "ind": "id", "tha": "th", "vie": "vi",
    "may": "ms", "msa": "ms", "slv": "sl", "slo": "sk", "slk": "sk", "est": "et",
    "lav": "lv", "lit": "lt", "cat": "ca", "baq": "eu", "eus": "eu", "glg": "gl",
    "alb": "sq", "sqi": "sq", "mac": "mk", "mkd": "mk", "bos": "bs", "ice": "is",
    "isl": "is", "aze": "az", "geo": "ka", "kat": "ka", "arm": "hy", "hye": "hy",
    "ben": "bn", "tam": "ta", "tel": "te", "urd": "ur", "kur": "ku",
    "turkish": "tr", "english": "en", "german": "de", "french": "fr", "spanish": "es",
    "italian": "it", "portuguese": "pt", "russian": "ru", "arabic": "ar",
    "türkçe": "tr",
    # OpenSubtitles.com's own regional codes.
    "pt-pt": "pt", "zh-cn": "zh", "zh-tw": "zh",
}


def canonical(code: str | None) -> str | None:
    """`tur` -> `tr`, `pt-BR` -> `pt-br`, `und`/empty -> None."""
    if not code:
        return None
    value = code.strip().lower().replace("_", "-")
    if not value or value in ("und", "unk", "mul", "zxx", "mis"):
        return None
    if value in _CANONICAL:
        return _CANONICAL[value]
    if len(value) == 2 or (len(value) == 5 and value[2] == "-"):
        return value
    return value
