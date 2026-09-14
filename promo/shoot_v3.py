#!/usr/bin/env python3
"""New app views, launcher, and shared-table interactions; all owned windows."""
import argparse
import json
import os
import sqlite3
import hashlib
import numpy as np
from PIL import Image
import time
from capture import ROOT
from scenes_v3 import V3, PHOTO_REF, prepare_v3
from remote_session import RemoteSession
from shoot_v2 import wait_ready


def take(name, layout, script='', window='1200x540', grid='9x6', second=None,
         warm='', motion=False, joined=False, real_disk=False):
    dest=V3/name
    prepare_v3(dest/'demo.db',layout,second=second)
    if joined:
        with sqlite3.connect(dest/'demo.db') as c:
            c.execute('UPDATE panel SET joined_to=1 WHERE id=2')
    path=dest/'take.txt'
    path.write_text('wait 1000\n'+warm+'key esc\nwait 1700\n'+script+'\nwait 30000\nquit\n')
    env=dict(os.environ,MAKEPAD_DISPLAY_LINK='0',MAKEPAD_NO_VSYNC='1',MAKEPAD_CAPTURE_BMP='1')
    if motion:
        env.update(SUPERAPP_CAPTURE_DIR=str(dest/'frames'),SUPERAPP_CAPTURE_SECONDS='8')
    with RemoteSession(dest/'demo.db',dest,window=window,
            extra_args=['--grid',grid,'--e2e',str(path),'--e2e-out',str(dest)]+([] if real_disk else ['--demo-disk']),
            env=env,binary=ROOT/'.context/promo/bin/superapp-capture') as s:
        wait_ready(s);s.shot('frame')
        if motion:
            start=s.record_gpu();seen=len((dest/'app.log').read_text().splitlines());cues=[]
            while not (dest/'frames/complete').exists():
                lines=(dest/'app.log').read_text().splitlines()
                for line in lines[seen:]:
                    if 'e2e:' in line:cues.append({'time':time.monotonic()-start,'action':line})
                seen=len(lines);time.sleep(.01)
            s.finish_gpu();s.shot('last')
            (dest/'cues.json').write_text(json.dumps(cues,indent=2)+'\n')
    if 'e2e: FAIL' in (dest/'app.log').read_text():raise RuntimeError(name+' failed')
    print('Captured',name,flush=True)


def media_viewer():
    # Native file loading needs the normal disk and blob capabilities. Every
    # account stays offline in this owned fixture; only its cached photo is read.
    dest=V3/'heroes/telegram-1'
    prepare_v3(dest/'demo.db',[[('media',['11','903'])]])
    with sqlite3.connect(dest/'demo.db') as c:
        c.execute('UPDATE account SET calendar_enabled=0,mail_enabled=0')
    with sqlite3.connect(dest/'blobs/index.db') as c:
        c.execute('CREATE TABLE blob(key TEXT PRIMARY KEY,file TEXT NOT NULL,size INTEGER NOT NULL,seq INTEGER NOT NULL)')
        name=hashlib.sha256(PHOTO_REF.encode()).hexdigest()
        c.execute('INSERT INTO blob VALUES(?,?,?,1)',(PHOTO_REF,name,(dest/'blobs'/name).stat().st_size))
    with RemoteSession(dest/'demo.db',dest,window='900x600',extra_args=['--grid','6x3'],
            env=dict(os.environ,MAKEPAD_DISPLAY_LINK='0',MAKEPAD_NO_VSYNC='1'),
            binary=ROOT/'.context/promo/bin/superapp-capture') as s:
        for _ in range(10):
            time.sleep(.5);s.shot('frame')
            pixels=np.asarray(Image.open(dest/'frame.png').convert('RGB')).astype(int)
            if np.count_nonzero(pixels.max(2)-pixels.min(2)>25)>50000:break
        else:raise RuntimeError('Photo viewer did not load')
    print('Captured Telegram photo viewer',flush=True)


def heroes():
    take('heroes/calendar-0',[[('calendar-month',['2026-09-01',''])]],window='1100x850',grid='7x6')
    take('heroes/calendar-1',[[('calendar',['','2026-09-14','Europe/Berlin'])],[('calendar-event',['113'])]],
         window='900x600',grid='8x6')
    take('heroes/telegram-0',[[('chats',[])],[('telegram-chat',['11'])]],window='900x600',grid='8x6')
    media_viewer()


def ui_launch():
    take('ui-launch',[[('inbox',[])],[('message',['1']),('calendar-event',['115'])]],grid='8x6',
         second=[[('calendar',[])],[('telegram-chat',['11'])]],
         warm='key cmd+2\nwait 1000\nkey cmd 2\nwait 350\nkey escape\nkey cmd+1\nwait 700\n',
         script='''key cmd 2
wait 480
type "cal"
wait 380
key down
wait 220
key enter
wait 700
key cmd+,
wait 620
key cmd+.
wait 700
''',motion=True)


def ui_mail():
    take('ui-mail',[[('inbox',[])],[('message',['1']),('calendar-event',['115'])]],grid='8x6',joined=True,
         # Preload both reader presentations before the accepted motion starts.
         warm='click "superapp panel model"\nwait 800\nclick "long version"\nwait 800\nclick "Q3 infra"\nwait 700\n',
         script='''click "filter"
type "@fr"
wait 360
click "@from"
wait 260
type "max "
wait 350
key esc
visible "superapp panel model"
click "superapp panel model"
wait 480
key space
key down
wait 480
key space
wait 700
''',motion=True)


def ui_files():
    take('ui-files',[[('files',['~/Downloads'])],[('file',['~/Downloads/README.txt'])],[('calendar',[])]],
         grid='8x3',joined=True,
         warm='click "report-q3.pdf"\nwait 800\nclick "README.txt"\nwait 650\nkey cmd+right 2\nwait 800\nkey cmd+left 2\nwait 600\n',
         script='''click "filter"
type "@kind:"
wait 550
type "pdf"
wait 450
key esc
key down
wait 550
key cmd+,
wait 420
key cmd+t
wait 480
key cmd+down
wait 700
''',motion=True)


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('scene',choices=['heroes','launch','mail','files']);a=p.parse_args()
    {'heroes':heroes,'launch':ui_launch,'mail':ui_mail,'files':ui_files}[a.scene]()
