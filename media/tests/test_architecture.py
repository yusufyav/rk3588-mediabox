"""Architecture proof.

The V2 media core exists because the previous approach treated the Stremio web
application as MediaBox's interface. These tests are what stops that coming
back: they are executable statements about what the media core may depend on.

They are deliberately crude — they read the source. A subtler test would be
easier to satisfy accidentally, which is the opposite of what is wanted here.
"""

from __future__ import annotations

import ast
import io
import tokenize
import unittest
from pathlib import Path

import media


PACKAGE_ROOT = Path(media.__file__).resolve().parent
TEST_DIR = PACKAGE_ROOT / "tests"


def code_only(path: Path) -> str:
    """The file with its comments and string literals removed.

    Prose is allowed to name Stremio — the streaming server really is
    Stremio's. What must not happen is code in the policy, inspector or proxy
    layers *depending* on it, and that is what is left after this.
    """
    text = path.read_text(encoding="utf-8")
    kept: list[str] = []
    try:
        for token in tokenize.generate_tokens(io.StringIO(text).readline):
            if token.type in (tokenize.COMMENT, tokenize.STRING):
                continue
            kept.append(token.string)
    except (tokenize.TokenError, IndentationError):  # pragma: no cover
        return text
    return " ".join(kept)


def source_files() -> list[Path]:
    return sorted(
        path
        for path in PACKAGE_ROOT.rglob("*.py")
        if TEST_DIR not in path.parents and path.parent != TEST_DIR
    )


#: Anything that would mean the media core is reading a rendered page rather
#: than an API. `window.` and `document.` are here as bare strings because a
#: Python file has no legitimate reason to contain either.
DOM_MARKERS = (
    "document.querySelector",
    "querySelectorAll",
    "getElementById",
    "getElementsByClassName",
    "innerHTML",
    "outerHTML",
    "window.core",
    "window.location",
    "addEventListener",
    "HTMLParser",
    "BeautifulSoup",
    "selenium",
    "playwright",
    "webdriver",
    "puppeteer",
    "chromium",
    "headless_browser",
    "<script",
    "<div",
)

#: Hosts and paths that only exist in the web application.
WEB_APP_MARKERS = (
    "stremio-web",
    "/hlsv2/",
    "shell.js",
    "shell.css",
    "/ui/",
    "CoreProvider",
    "deepLinks",
    "externalPlayer",
)


class NoWebApplicationDependency(unittest.TestCase):
    def test_no_module_touches_a_dom_or_drives_a_browser(self):
        offenders: list[str] = []
        for path in source_files():
            text = path.read_text(encoding="utf-8")
            for marker in DOM_MARKERS:
                if marker in text:
                    offenders.append(f"{path.relative_to(PACKAGE_ROOT)}: {marker!r}")
        self.assertEqual(offenders, [], "the media core must never read a rendered page")

    def test_no_module_references_the_stremio_web_application(self):
        """Checked against code only: prose may say which surfaces are avoided."""
        offenders: list[str] = []
        for path in source_files():
            text = code_only(path)
            for marker in WEB_APP_MARKERS:
                if marker in text:
                    offenders.append(f"{path.relative_to(PACKAGE_ROOT)}: {marker!r}")
        self.assertEqual(
            offenders,
            [],
            "the media core must not depend on the Stremio web application's surfaces",
        )

    def test_the_media_core_imports_nothing_from_the_control_plane(self):
        """Codex owns mediaboxd. The media core must be embeddable, not entangled."""
        forbidden = {"mediaboxd", "webui", "rust"}
        offenders: list[str] = []
        for path in source_files():
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
            for node in ast.walk(tree):
                if isinstance(node, ast.Import):
                    names = [alias.name.split(".")[0] for alias in node.names]
                elif isinstance(node, ast.ImportFrom):
                    names = [(node.module or "").split(".")[0]]
                else:
                    continue
                for name in names:
                    if name in forbidden:
                        offenders.append(f"{path.relative_to(PACKAGE_ROOT)}: imports {name}")
        self.assertEqual(offenders, [])

    def test_the_media_core_has_no_third_party_dependency(self):
        """It runs on the appliance's stock Python; nothing is installed for it."""
        allowed_stdlib_prefixes = {
            "argparse", "ast", "collections", "dataclasses", "enum", "fractions",
            "gzip", "http", "ipaddress", "json", "logging", "os", "pathlib", "queue",
            "re", "secrets", "shutil", "signal", "socket", "stat", "subprocess", "sys",
            "tempfile", "textwrap", "threading", "time", "typing", "unittest",
            "urllib", "zlib", "math", "functools", "itertools", "contextlib", "io",
            "media", "__future__",
        }
        offenders: list[str] = []
        for path in source_files():
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
            for node in ast.walk(tree):
                if isinstance(node, ast.Import):
                    names = [alias.name.split(".")[0] for alias in node.names]
                elif isinstance(node, ast.ImportFrom):
                    if node.level:  # relative import, inside the package
                        continue
                    names = [(node.module or "").split(".")[0]]
                else:
                    continue
                for name in names:
                    if name and name not in allowed_stdlib_prefixes:
                        offenders.append(f"{path.relative_to(PACKAGE_ROOT)}: {name}")
        self.assertEqual(offenders, [])


class ProviderBoundary(unittest.TestCase):
    """Stremio must be reachable only through the adapter."""

    def test_only_the_stremio_package_knows_the_stremio_api_host(self):
        offenders = [
            str(path.relative_to(PACKAGE_ROOT))
            for path in source_files()
            if "strem.io" in path.read_text(encoding="utf-8")
            and path.parent.name != "stremio"
        ]
        self.assertEqual(offenders, [])

    def test_the_policy_layer_has_no_code_dependency_on_the_provider(self):
        for area in ("policy", "inspector", "proxy"):
            for path in (PACKAGE_ROOT / area).rglob("*.py"):
                with self.subTest(path=str(path.relative_to(PACKAGE_ROOT))):
                    self.assertNotIn("stremio", code_only(path).lower())

    def test_the_public_api_route_names_do_not_name_the_provider(self):
        from ..api import MediaCore

        core_source = (PACKAGE_ROOT / "api.py").read_text(encoding="utf-8")
        docstring = core_source.split('"""')[1]
        routes = [
            part
            for line in docstring.splitlines()
            if line.strip().startswith(("GET", "POST", "DELETE"))
            for part in line.split()
            if part.startswith("/media")
        ]
        self.assertTrue(routes, "the API docstring should list its routes")
        for route in routes:
            self.assertNotIn("stremio", route.lower(), f"route names the provider: {route}")
        self.assertTrue(MediaCore is not None)


class CapabilityProfileIsSingleSourceOfTruth(unittest.TestCase):
    def test_codec_names_are_not_hard_coded_outside_the_profile(self):
        """A stray `codec == "hevc"` is how a policy stops being reviewable."""
        offenders: list[str] = []
        scanned = [
            PACKAGE_ROOT / "policy" / "decide.py",
            PACKAGE_ROOT / "policy" / "ranking.py",
            PACKAGE_ROOT / "policy" / "video.py",
        ]
        for path in scanned:
            for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
                stripped = line.strip()
                if stripped.startswith("#") or stripped.startswith('"'):
                    continue
                for codec in ('"hevc"', '"h264"', '"av1"', '"vp9"'):
                    if codec in stripped:
                        offenders.append(f"{path.name}:{number}: {stripped}")
        self.assertEqual(offenders, [])

    def test_every_capability_claim_names_its_evidence(self):
        from ..policy.capabilities import PROFILES

        for name, profile in PROFILES.items():
            with self.subTest(profile=name):
                self.assertTrue(profile.evidence, "a profile with no evidence is a guess")
                for reference in profile.evidence:
                    self.assertTrue(reference.startswith("results/"))

    def test_dolby_vision_is_not_claimed_on_the_production_profile(self):
        from ..policy.capabilities import RK3588_ORANGEPI5_PRODUCTION

        self.assertFalse(RK3588_ORANGEPI5_PRODUCTION.video.dolby_vision_pipeline)


if __name__ == "__main__":
    unittest.main()
