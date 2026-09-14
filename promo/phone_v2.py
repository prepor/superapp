#!/usr/bin/env python3
"""A dynamic native phone-layout preview, explicitly not an Android recording."""
import os
from capture import ROOT
from scenes import V2, prepare
from remote_session import RemoteSession
from shoot_v2 import wait_ready


def main():
    dest=V2/'phone'
    prepare(dest/'demo.db',[[('inbox',[])],[('rss',[])],[('chats',[])],[('notes',[])]],
            second=[[('editor',['note','1'])],[('rss-article',['2'])]])
    script=dest/'take.txt'
    script.write_text('''wait 1000
key cmd+2
wait 700
key cmd+right
wait 700
key cmd+left
wait 400
key cmd+1
wait 700
pan2 0 -260
wait 600
pan2 0 260
wait 600
key esc
wait 2000
pan2 0 -260
wait 1000
holdmove "inbox" 120 0
wait 650
pan2 0 260
wait 650
key cmd+]
wait 700
key cmd+w
wait 650
pan2 0 -260
wait 650
click "rss"
wait 650
click "Berlin by bike"
wait 850
key cmd+2
wait 850
key cmd+1
wait 850
pan2 0 -260
wait 850
pan2 0 260
wait 650
pan2 -330 0
wait 750
pan2 330 0
wait 750
pan2 0 -260
wait 30000
quit
''')
    env=dict(os.environ,SUPERAPP_CAPTURE_DIR=str(dest/'frames'),SUPERAPP_CAPTURE_SECONDS='15',
             MAKEPAD_DISPLAY_LINK='0',MAKEPAD_NO_VSYNC='1')
    binary=ROOT/'.context/promo/bin/superapp-capture' if env.get('MAKEPAD_CAPTURE_BMP') else ROOT/'target/release/superapp'
    with RemoteSession(dest/'demo.db',dest,window='480x850',
            extra_args=['--grid','4x3','--e2e',str(script),'--demo-disk','--e2e-out',str(dest)],
            env=env,binary=binary) as s:
        wait_ready(s);s.shot('ready');s.record_gpu();s.finish_gpu();s.shot('last')
    log=(dest/'app.log').read_text()
    if 'e2e: FAIL' in log: raise RuntimeError('Phone choreography failed; inspect app.log')
    print('Recorded dynamic phone-layout preview',flush=True)


if __name__=='__main__':main()
