#!/usr/bin/env python3
"""High-resolution, fully framed panels and measured native motion takes."""
import argparse
import json
import os
import time
from capture import ROOT, OUT
from scenes import V2, BASE, HEROES, CHOREOGRAPHY, SECOND, prepare
from remote_session import RemoteSession


def wait_file(path, seconds=30):
    until=time.monotonic()+seconds
    while not path.exists():
        if time.monotonic()>until:
            raise TimeoutError(str(path))
        time.sleep(.05)


def wait_ready(session, seconds=60):
    until=time.monotonic()+seconds
    while 'e2e: key esc ×1' not in (session.dest/'app.log').read_text():
        if session.proc.poll() is not None or time.monotonic()>until:
            raise TimeoutError('The prepared scene did not reach its ready marker')
        time.sleep(.05)
    time.sleep(.12)


def stills(names):
    for name in names:
        for i, panel in enumerate(HEROES[name]):
            dest=V2/'heroes'/f'{name}-{i}'
            source=V2/'live-language/demo.db' if name=='agents' else BASE
            prepare(dest/'demo.db', [[panel]], source=source)
            script=dest/'take.txt'
            body='wait 800\n'
            if name=='terminal':
                body+='type "echo hello, superapp"\nkey enter\nwait 400\ntype "stty size"\nkey enter\nwait 300\n'
            body+='key esc\nwait 30000\nquit\n'
            script.write_text(body)
            with RemoteSession(dest/'demo.db', dest, window='720x480',
                    extra_args=['--grid','6x4','--e2e',str(script),'--demo-disk','--e2e-out',str(dest)],
                    env=dict(os.environ,MAKEPAD_DISPLAY_LINK='0',MAKEPAD_NO_VSYNC='1'),
                    binary=ROOT/'target/release/superapp') as s:
                wait_ready(s);s.shot('frame')
            print('Captured',name,i,flush=True)


def motion(name, gpu=True):
    dest=V2/name
    source=BASE
    if name=='choreography':
        layout=CHOREOGRAPHY
        focus=2
        events=[(.15,'Comma','⌘,'), (1.65,'Period','⌘.'),
                (3.15,'RBracket','⌘]'), (4.65,'KeyT','⌘T'),
                (6.0,'ArrowUp','⌘↑'), (7.3,'Key2','⌘2'), (9.0,'Key1','⌘1')]
        warm='key cmd+2\nwait 1200\nkey cmd+1\nwait 1000\nkey cmd+2\nwait 800\nkey cmd+1\nwait 1000\n'
    elif name=='android-desktop':
        source=V2/'live-language/demo.db'
        layout=[[('rss-article',['2'])],[('lesson',['101'])]]
        focus=1
        events=[(.15,'Comma','⌘,'),(1.65,'Period','⌘.'),(3.15,'RBracket','⌘]'),
                (4.65,'KeyT','⌘T'),(6,'ArrowUp','⌘↑'),(7.3,'KeyT','⌘T'),
                (8.8,'LBracket','⌘['),(10.3,'Comma','⌘,')]
        warm='key cmd+,\nwait 600\nkey cmd+.\nwait 600\n'
    else:
        kinds=[('inbox',[]),('chats',[]),('rss',[]),('notes',[])]
        layout=[[('editor',['note','1'])]]+[[kinds[i%4]] for i in range(32)]
        focus=1
        events=[(.05+i*.078,'RBracket','⌘]') for i in range(64)]
        events += [(5.05+i*.078,'LBracket','⌘[') for i in range(64)]
        warm=('key cmd+]\nwait 75\n'*64+'wait 500\n'+
              'key cmd+[\nwait 75\n'*64+'wait 1000\n')
    prepare(dest/'demo.db',layout,second=SECOND,focus=focus,source=source)
    script=dest/'take.txt'
    script.write_text('wait 1000\n'+warm+'key esc\nwait 60000\nquit\n')
    env=dict(os.environ)
    env.pop('SUPERAPP_FRAME_LOG',None)
    binary=ROOT/'target/release/superapp'
    if os.environ.get('MAKEPAD_CAPTURE_BMP'):
        binary=OUT/'bin/superapp-capture'
    if gpu:
        env.update(SUPERAPP_CAPTURE_DIR=str(dest/'frames'),SUPERAPP_CAPTURE_SECONDS='12',
                   MAKEPAD_DISPLAY_LINK='0',MAKEPAD_NO_VSYNC='1')
    extra_grid=['--grid','10x6'] if name=='android-desktop' else []
    with RemoteSession(dest/'demo.db',dest,window='1200x540',
            extra_args=extra_grid+['--e2e',str(script),'--demo-disk','--e2e-out',str(dest)],
            env=env, binary=binary) as s:
        wait_ready(s);s.shot('ready')
        start=s.record_gpu() if gpu else s.record_precise(12)
        cues=[]
        for at,code,label in events:
            time.sleep(max(0,at-(time.monotonic()-start)))
            sent=time.monotonic()-start
            s.call('k',c=code,cmd=1)
            cues.append({'time':sent,'key':label})
        (dest/'cues.json').write_text(json.dumps(cues,indent=2)+'\n')
        if gpu: s.finish_gpu()
        elif s.recording: s.recording.wait()
        s.shot('last')
    print('Recorded',name,flush=True)


if __name__=='__main__':
    p=argparse.ArgumentParser()
    p.add_argument('what',choices=['stills','choreography','speed','android-desktop'])
    p.add_argument('--only',nargs='+')
    p.add_argument('--window-capture',action='store_true')
    a=p.parse_args()
    if a.what=='stills': stills(a.only or list(HEROES))
    else: motion(a.what,not a.window_capture)
