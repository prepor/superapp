#!/usr/bin/env python3
"""Capture real sync between two local app instances, one at phone dimensions."""
import io
import json
import os
import sqlite3
import time
from PIL import Image
from capture import OUT, launch
from remote_session import RemoteSession


def wait_for(test, label, seconds=30):
    until = time.monotonic()+seconds
    while not test():
        if time.monotonic() > until:
            raise TimeoutError(label)
        time.sleep(.1)


def edit(session, text, width):
    im = Image.open(io.BytesIO(session.call('g', raw=1))).convert('RGB')
    scale = im.width/width
    px = im.load()
    start = next(x for x in range(im.width-100)
                 if all(max(px[x+i, round(20*scale)]) < 45 for i in range(100)))
    session.call('click', x=start/scale+20, y=95)
    session.call('k', c='KeyA', cmd=1)
    session.call('t', t=text)
    session.call('k', c='Escape')


def main():
    a = OUT/'native/sync-a'
    b = OUT/'native/sync-b'
    for dest in (a, b):
        dest.mkdir(parents=True, exist_ok=True)
        if (dest/'demo.db').exists():
            raise RuntimeError('Archive the existing sync take before recording again')
    ticket = a/'ticket'
    initial = 'Launch prep\n\nOne workspace.\nRoom to think.'
    changed_a = 'Launch prep\n\nDocs ready.\nAndroid clip pending.'
    changed_b = 'Launch prep\n\nDocs ready.\nAndroid clip ready.'
    script_a = (launch('device sync')+'wait 500\n'+launch('notes')+
                'click "new note"\nwait 350\nclick "editor"\n'+
                'paste '+json.dumps(initial)+'\nwait 500\nkey esc\nshot ready\nwait 120000\nquit\n')
    (a/'script.txt').write_text(script_a)
    env = dict(os.environ, SUPERAPP_SYNC='loopback', SUPERAPP_E2E_TICKET_OUT=str(ticket))
    with RemoteSession(a/'demo.db', a, extra_args=[
            '--e2e', str(a/'script.txt'), '--e2e-out', str(a)], env=env) as sa:
        wait_for(lambda: ticket.exists() and ticket.stat().st_size, 'No pairing ticket')
        script_b = (launch('device sync')+'wait 500\nclick "pair with"\nwait 200\n'+
                    'type '+json.dumps(ticket.read_text().strip())+'\nwait 250\nclick "pair"\nwait 3500\n'+
                    launch('notes')+'wait 500\nclick "Launch prep"\nwait 600\nshot ready\nwait 120000\nquit\n')
        (b/'script.txt').write_text(script_b)
        env_b = dict(os.environ, SUPERAPP_SYNC='loopback')
        with RemoteSession(b/'demo.db', b, window='480x850', extra_args=[
                '--grid', '4x3', '--e2e', str(b/'script.txt'), '--e2e-out', str(b)], env=env_b) as sb:
            wait_for(lambda: (a/'ready.png').exists() and (b/'ready.png').exists(), 'Pairing did not finish')
            ca = sqlite3.connect(f'file:{a / "demo.db"}?mode=ro', uri=True)
            cb = sqlite3.connect(f'file:{b / "demo.db"}?mode=ro', uri=True)
            def has(c, body):
                return c.execute('SELECT uid FROM notes_note WHERE body = ?', (body,)).fetchone()
            assert has(ca, initial) and has(cb, initial), 'Initial note did not cross'
            started_a = time.monotonic()
            sa.record(13)
            started_b = time.monotonic()
            sb.record(13)
            time.sleep(1)
            edit(sa, changed_a, 1440)
            wait_for(lambda: has(cb, changed_a), 'Desktop edit did not arrive', 10)
            arrived_b = time.monotonic()
            time.sleep(1.5)
            edit(sb, changed_b, 480)
            wait_for(lambda: has(ca, changed_b), 'Phone-layout edit did not arrive', 10)
            arrived_a = time.monotonic()
            sa.shot('synced')
            sb.shot('synced')
            (OUT/'native/sync-result.json').write_text(json.dumps({
                'kind': 'two native Mac processes, separate stores, loopback transport',
                'android_device': False, 'widths': [1440, 480],
                'recording_launch_offset': started_b-started_a,
                'first_edit_received_at': arrived_b-started_a,
                'second_edit_received_at': arrived_a-started_a,
                'note_uid': has(ca, changed_b)[0],
                'same_note_uid': has(ca, changed_b) == has(cb, changed_b),
                'final_body': changed_b,
                'peer_counts': [c.execute('SELECT COUNT(*) FROM sync_peer WHERE removed = 0').fetchone()[0]
                                for c in (ca, cb)]}, indent=2)+'\n')
            print('Both edits crossed automatically between the two stores.', flush=True)
            ca.close()
            cb.close()


if __name__ == '__main__':
    main()
