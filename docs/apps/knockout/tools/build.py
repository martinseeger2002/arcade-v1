#!/usr/bin/env python3
"""build.py - one self-contained file: dist/knockout.html (code + sprites inline as data URIs, no outside requests)."""
import base64, json
from pathlib import Path
R = Path(__file__).resolve().parent.parent
ROSTER = ['buster', 'lester', 'shiba', 'pep3d', 'digi', 'arcade']
SHEETS = ROSTER + [w + '-left' for w in ROSTER if (R / 'assets' / f'{w}-left.png').exists()]   # left-facing sheets keep letters readable
spr = {w: {'src': 'data:image/png;base64,' + base64.b64encode((R / 'assets' / f'{w}.png').read_bytes()).decode(),
           'meta': json.loads((R / 'assets' / f'{w}.json').read_text())} for w in SHEETS}
for m in spr.values(): m['meta'].pop('who', None)
code = '\n'.join((R / 'src' / f).read_text() for f in ['core.js', 'data.js', 'stages.js', 'game.js', 'boss.js', 'story.js', 'net.js', 'ui.js', 'main.js'])
html = (R / 'src' / 'index.html').read_text().replace('/*SPRITES*/', 'const SPRITE_DATA = ' + json.dumps(spr, separators=(',', ':')) + ';').replace('/*CODE*/', code)
out = R / 'dist' / 'knockout.html'; out.parent.mkdir(exist_ok=True); out.write_text(html)
print(out, len(html.encode()) // 1024, 'KB')
