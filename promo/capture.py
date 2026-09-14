#!/usr/bin/env python3
"""Capture deterministic Superapp UI footage in isolated demo stores."""
import argparse
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / '.context/promo'


def launch(name):
    return f'key cmd 2\nwait 150\ntype {json.dumps(name)}\nwait 200\nkey enter\nwait 500\n'


def shot(name):
    return f'wait 250\nshot {name}\n'


def scripts():
    jobs = {}
    jobs['mail'] = 'wait 800\n' + shot('mail-list') + 'click "Q3 infra"\n' + shot('mail-thread')
    jobs['files'] = launch('files') + shot('files-list') + 'click "go to"\ntype "~/Downloads/"\nkey enter\nwait 350\n' + shot('files-downloads') + 'click "report-q3.pdf"\n' + shot('files-card')
    jobs['calendar'] = launch('calendar') + shot('calendar-list') + 'click "month"\n' + shot('calendar-month') + 'click "day 2026-09-02"\n' + shot('calendar-day')
    jobs['workshop'] = launch('projects') + 'click "superapp"\nwait 350\nclick "zurich"\nkey enter\n' + shot('workshop-workspace') + 'click "review"\nwait 450\nclick "src/review/anchors.rs"\nkey enter\n' + shot('workshop-review')
    jobs['terminal'] = launch('terminal') + shot('terminal-empty') + 'type "printf \'hello, superapp\\n\'"\nkey enter\nwait 500\n' + shot('terminal-output')
    jobs['rss'] = launch('rss') + shot('rss-list') + 'key down\nwait 400\n' + shot('rss-article')
    jobs['notes'] = launch('notes') + 'click "new note"\nwait 350\nclick "editor"\ntype "Launch prep\\n\\nReview at 16:00\\nDocs ready\\nAndroid clip pending"\n' + shot('notes-prep') + 'type "\\nDemo ready."\n' + shot('notes-desktop-edit') + 'type "\\nLet\u0027s ship."\n' + shot('notes-phone-edit')
    jobs['agents'] = 'wait 500\nkey cmd+shift+a\nwait 400\ntype "Prep me for this meeting. Save a note."\n' + shot('agents-context')
    jobs['agent-tool'] = launch('files') + 'click "go to"\ntype "~/Downloads/"\nkey enter\nwait 300\n' + launch('new chat') + 'type "rename the readme"\nkey enter\nwait 1200\n' + shot('agents-tool')
    jobs['telegram'] = launch('chats') + shot('telegram-list') + 'click "stelaxis"\nwait 500\n' + shot('telegram-chat')
    jobs['fluent'] = launch('fluent') + shot('fluent-desk') + 'click "start"\n' + shot('fluent-exercise') + 'click "bin"\n' + shot('fluent-answer')
    jobs['choreography'] = launch('notes') + 'click "new note"\nwait 250\nclick "editor"\ntype "Launch prep\\n\\nOne place for everything."\nkey esc\nwait 400\n' + launch('calendar') + launch('rss') + shot('workspace') + 'key cmd+,\nwait 550\nkey cmd+.\nwait 550\nkey cmd+]\nwait 550\nkey cmd+t\nwait 700\n' + shot('workspace-tabs') + 'key cmd+2\nwait 600\nkey cmd+1\nwait 600\n' + shot('workspace-return')
    jobs['phone-preview'] = launch('notes') + 'click "new note"\nwait 250\nclick "editor"\ntype "Launch prep\\n\\nReview at 16:00\\nDocs ready\\nAndroid clip pending"\nkey esc\n' + shot('phone-note') + 'pan2 0 -260\nwait 500\n' + shot('phone-overview')
    return jobs


def capture(name, body, binary, validate=False):
    dest = OUT / 'captures' / name
    dest.mkdir(parents=True, exist_ok=True)
    script = dest / 'script.txt'
    lines = body.splitlines()
    settled = []
    for i, line in enumerate(lines):
        if line.startswith('type ') and '\\n' in line:
            line = 'paste ' + line[5:]
        settled.append(line)
        if line.startswith(('click ', 'key ', 'type ', 'paste ')) and (i + 1 == len(lines) or not lines[i + 1].startswith('wait ')):
            settled.append('wait 180')
    script.write_text('# args: --demo-disk\n# env:\nwait 350\n' + '\n'.join(settled) + '\nwait 350\nquit\n')
    frames = dest / 'frames'
    frames.mkdir(exist_ok=True)
    env = dict(os.environ, MAKEPAD_HEADLESS_DPI='1', MAKEPAD_HEADLESS_OUT_DIR=str(frames))
    args = ['mise', 'exec', '--', str(binary), '--e2e', str(script), '--demo-disk',
            '--e2e-out', str(dest), '--window', '480x850' if name == 'phone-preview' else '1440x900',
            '--draws', '12000']
    if name == 'phone-preview':
        args += ['--grid', '4x3']
    if validate:
        args += ['--no-draw']
    logfile = dest / ('validate.log' if validate else 'capture.log')
    print(f'{"Validating" if validate else "Capturing"} {name}', flush=True)
    with logfile.open('w') as log:
        result = subprocess.run(args, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
    if result.returncode:
        print(logfile.read_text()[-6000:], flush=True)
        raise RuntimeError(f'{name} failed: {logfile}')
    if not validate:
        images = sorted(p.name for p in dest.glob('*.png'))
        if not images:
            raise RuntimeError(f'{name} produced no screenshots; see {logfile}')
        (dest / 'manifest.json').write_text(json.dumps({
            'name': name, 'kind': 'deterministic demo capture',
            'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
            'images': images, 'script': 'script.txt',
            'limitations': 'Software-rendered demo fixtures; not a runtime performance measurement or a live model response.'
        }, indent=2) + '\n')
        print(f'  {len(images)} screenshots, {len(list(frames.glob("*.png")))} rendered frames', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=OUT / 'bin/superapp-headless')
    parser.add_argument('--only', nargs='+')
    parser.add_argument('--validate', action='store_true')
    args = parser.parse_args()
    for name, body in scripts().items():
        if not args.only or name in args.only:
            capture(name, body, args.binary.resolve(), args.validate)


if __name__ == '__main__':
    main()
