#!/usr/bin/env python3
"""Salt okunur repo taraması; yalnız atlas/snapshot.js çıktısını yeniler.
Kurasyon atlas-data.js içindedir; bu araç mimari kararlarını kendiliğinden değiştirmez.
"""
import hashlib
import json
import re
import subprocess
from datetime import datetime
from pathlib import Path
from zoneinfo import ZoneInfo

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

def git(*args):
    return subprocess.check_output(['git', '-C', str(ROOT), *args], text=True).strip()

paths = [p for p in git('ls-files').splitlines() if not p.startswith('docs/project-atlas/') and (ROOT / p).is_file()]
# Yalnız depoda izlenen mimari belgeleri ve kürasyonun atıf yaptığı kaynaklar.
curation = (HERE / 'atlas-data.js').read_text()
refs = set(re.findall(r"'((?:rust/|media/|packaging/|config/|scripts/|tools/|tests/|src/|docs/|results/)[^'\s]+|README\.md|CMakeLists\.txt)'", curation))
docs = [p for p in paths if p.endswith('.md') and (p == 'README.md' or p.startswith(('docs/', 'results/', 'tools/', 'patches/')))]
selected = sorted(set(docs) | refs)
sources = {}
missing = []
for path in selected:
    file = ROOT / path
    if not file.is_file():
        missing.append(path)
        continue
    content = file.read_text(encoding='utf-8')
    sources[path] = {'text': content, 'sha256': hashlib.sha256(content.encode()).hexdigest()[:12], 'lines': len(content.splitlines())}
if missing:
    raise SystemExit('Bulunamayan kaynak: ' + ', '.join(missing))
services = []
for file in sorted((ROOT / 'packaging/systemd').glob('*.service')):
    raw = file.read_text()
    fields = {}
    joined = raw.replace('\\\n', ' ')
    for line in joined.splitlines():
        if '=' in line and not line.lstrip().startswith('#'):
            key, value = line.split('=', 1)
            fields.setdefault(key, []).append(value.strip())
    services.append({'name':file.name, 'path':str(file.relative_to(ROOT)), 'fields':fields})
    path = str(file.relative_to(ROOT))
    sources[path] = {'text':raw, 'sha256':hashlib.sha256(raw.encode()).hexdigest()[:12], 'lines':len(raw.splitlines())}
inventory = [{'path':p,'bytes':(ROOT/p).stat().st_size,'ext':Path(p).suffix or '(uzantısız)'} for p in paths]
result = {'generated':datetime.now(ZoneInfo('Europe/Istanbul')).isoformat(timespec='seconds'), 'revision':git('rev-parse', '--short', 'HEAD'), 'revisionFull':git('rev-parse','HEAD'), 'dirty':bool(git('status','--porcelain','--untracked-files=no')), 'documents':docs, 'sources':sources, 'services':services, 'inventory':inventory}
(HERE / 'snapshot.js').write_text('/* refresh_snapshot.py tarafından üretildi. */\nwindow.SNAPSHOT = '+json.dumps(result,ensure_ascii=False,separators=(',',':'))+';\n')
print(f'{len(sources)} kaynak, {len(docs)} belge, {len(services)} servis, {len(inventory)} dosya → snapshot.js')
