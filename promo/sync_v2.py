#!/usr/bin/env python3
"""Real bidirectional sync, with a complete note viewport on each side."""
import json
import os
import sqlite3
import time
from capture import OUT, ROOT, launch
from scenes import V2, prepare
from remote_session import RemoteSession
from native_sync import wait_for


def main():
    a=V2/'sync-a';b=V2/'sync-b'
    initial='Launch checklist\n\n[x] Documentation\n[x] Setup guides\n[x] Desktop walkthrough\n\nAndroid clip: recording'
    first=initial.replace('recording','ready')
    second=first+'\n\nReady to share.'
    for name,dest in [('sync-a',a),('sync-b',b)]:
        source=OUT/'native'/name/'demo.db'
        c=sqlite3.connect(f'file:{source}?mode=ro',uri=True)
        note=c.execute("SELECT id FROM notes_note WHERE title='Launch prep'").fetchone()[0];c.close()
        prepare(dest/'demo.db',[[('editor',['note',str(note)])]],source=source)
        c=sqlite3.connect(dest/'demo.db')
        for table in ('sync_op','sync_have','sync_self','sync_peer','sync_link'):
            c.execute('DELETE FROM '+table)
        # A prepared starting state, before pairing or editing on camera.
        c.execute('UPDATE notes_note SET title=?,body=? WHERE id=?',('Launch checklist',initial,note))
        c.commit();c.close()
    ticket=a/'ticket'
    sa_script=a/'take.txt'
    sa_script.write_text(launch('device sync')+'wait 600\nshot ready\nwait 120000\nquit\n')
    env=dict(os.environ,SUPERAPP_SYNC='loopback',SUPERAPP_E2E_TICKET_OUT=str(ticket),
             SUPERAPP_CAPTURE_DIR=str(a/'frames'),SUPERAPP_CAPTURE_SECONDS='9',
             MAKEPAD_DISPLAY_LINK='0',MAKEPAD_NO_VSYNC='1')
    with RemoteSession(a/'demo.db',a,window='1000x550',extra_args=['--grid','6x4',
            '--e2e',str(sa_script),'--e2e-out',str(a)],env=env,binary=ROOT/'target/release/superapp') as sa:
        wait_for(lambda:ticket.exists() and ticket.stat().st_size,'No sync ticket')
        sb_script=b/'take.txt'
        sb_script.write_text(launch('device sync')+'click "pair with"\nwait 200\n'+
            'type '+json.dumps(ticket.read_text().strip())+'\nwait 200\nclick "pair"\nwait 2200\n'+
            'key cmd+w\nwait 600\nshot ready\nwait 120000\nquit\n')
        env_b=dict(env,SUPERAPP_CAPTURE_DIR=str(b/'frames'));env_b.pop('SUPERAPP_E2E_TICKET_OUT')
        with RemoteSession(b/'demo.db',b,window='480x850',extra_args=['--grid','4x3',
                '--e2e',str(sb_script),'--e2e-out',str(b)],env=env_b,binary=ROOT/'target/release/superapp') as sb:
            wait_for(lambda:(a/'ready.png').exists() and (b/'ready.png').exists(),'Sync views not ready')
            ca=sqlite3.connect(f'file:{a/"demo.db"}?mode=ro',uri=True)
            cb=sqlite3.connect(f'file:{b/"demo.db"}?mode=ro',uri=True)
            wait_for(lambda: all(c.execute('SELECT COUNT(*) FROM sync_peer WHERE removed=0').fetchone()[0]==2
                                for c in (ca,cb)), 'Devices did not pair')
            # Keep the invitation open until the second instance has paired.
            sa.call('k',c='KeyW',cmd=1);time.sleep(.6);sa.shot('ready-final')
            def has(c,body):return c.execute('SELECT uid FROM notes_note WHERE body=?',(body,)).fetchone()
            def edit(s,body):
                s.call('click',x=100,y=130);s.call('k',c='KeyA',cmd=1);s.call('t',t=body);s.call('k',c='Escape')
            assert has(ca,initial) and has(cb,initial)
            t0=sa.record_gpu();tb=sb.record_gpu()
            time.sleep(.75)
            sent_a=time.monotonic();edit(sa,first)
            wait_for(lambda:has(cb,first),'First edit did not cross',10)
            arrived_b=time.monotonic()
            time.sleep(1.3)
            sent_b=time.monotonic();edit(sb,second)
            wait_for(lambda:has(ca,second),'Return edit did not cross',10)
            arrived_a=time.monotonic()
            result={'kind':'two native Mac windows, separate stores, loopback sync',
                    'android_device':False,'same_note_uid':has(ca,second)==has(cb,second),
                    'start_offset':tb-t0,'first_sent_at':sent_a-t0,'first_received_at':arrived_b-t0,
                    'second_sent_at':sent_b-t0,'second_received_at':arrived_a-t0,
                    'note_uid':has(ca,second)[0], 'final_body':second}
            (V2/'sync-result.json').write_text(json.dumps(result,indent=2)+'\n')
            sa.finish_gpu();sb.finish_gpu();sa.shot('last');sb.shot('last')
            ca.close();cb.close()
            print('Both edits synced automatically',flush=True)


if __name__=='__main__':main()
