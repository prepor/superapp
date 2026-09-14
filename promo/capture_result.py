#!/usr/bin/env python3
"""Open an existing live agent result in a background demo window."""
import argparse
import json
import sqlite3
import time
from capture import OUT
from remote_session import RemoteSession


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('scene', choices=['note', 'lesson'])
    scene = parser.parse_args().scene
    source = OUT/('live-agent' if scene == 'note' else 'live-language')
    dest = OUT/'native'/f'agent-{scene}'
    dest.mkdir(parents=True, exist_ok=True)
    db = dest/'result-view.db'
    c = sqlite3.connect(f'file:{source / "demo.db"}?mode=ro', uri=True)
    target = sqlite3.connect(db)
    c.backup(target)
    c.close()
    if scene == 'note':
        result = json.loads((source/'result.json').read_text())
        kind, args = 'editor', ['note', str(result['notes'][-1][0])]
    else:
        result = json.loads((source/'result.json').read_text())
        kind, args = 'lesson', [str(result['lessons'][-1]['id'])]
    target.executescript('''
        DELETE FROM effect;
        DELETE FROM panel WHERE id != 1;
        DELETE FROM ws_col WHERE ws != 0 OR idx != 0;
        UPDATE workspace SET focus = 1 WHERE k = 0;
        UPDATE wm SET active = 0;
    ''')
    target.execute('UPDATE panel SET kind = ?, args = ? WHERE id = 1', (kind, json.dumps(args)))
    target.commit()
    target.close()
    script = 'wait 500\nshot ready\nwait 2500\n'
    if scene == 'lesson':
        answer = json.loads(result['exercises'][0]['accepted'])[0]
        script += 'click '+json.dumps(answer)+'\nwait 600\nshot answer\n'
    script += 'wait 7000\nquit\n'
    (dest/'view.txt').write_text(script)
    with RemoteSession(db, dest, extra_args=[
            '--e2e', str(dest/'view.txt'), '--e2e-out', str(dest)]) as session:
        session.record(7)
        time.sleep(.2)
        session.shot(scene)
    print(dest, flush=True)


if __name__ == '__main__':
    main()
