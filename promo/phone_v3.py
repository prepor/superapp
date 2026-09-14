#!/usr/bin/env python3
"""Frame-paced input through the same Overview touch paths used on Android."""
import os
import json
import time
from capture import ROOT
from scenes_v3 import V3, prepare_v3
from remote_session import RemoteSession
from shoot_v2 import wait_ready


def main():
    dest=V3/'phone'
    prepare_v3(dest/'demo.db',
        [[('inbox',[]),('chats',[]),('rss',[])],
         [('calendar',[]),('editor',['note','1']),('files',['~/Downloads'])],
         [('workshop_workspaces',[]),('terminal',[])]],
        second=[[('rss-article',['2']),('lesson',['100'])],[('telegram-chat',['11'])],[('notes',[])]])
    script=dest/'take.txt'
    script.write_text('''wait 1200
key cmd+2
wait 700
key cmd+right
wait 700
key cmd+1
wait 700
pan2 0 -260
wait 400
click "workspace 2"
wait 400
click "workspace 1"
wait 400
pan2 0 260
wait 500
key esc
wait 1700
gesture-ms 360
pan2 0 -260
wait 280
click "workspace 2"
wait 440
click "workspace 1"
wait 400
gesture-ms 850
holdmove "inbox" 177.6 60 hold
wait 220
accel "place in column 2 row 2" -
drop
wait 480
accel "overview" -
gesture-ms 520
swipe "rss" 0 130
wait 420
absent "rss"
click "workspace 2"
wait 400
click "Berlin cycling"
wait 700
gesture-ms 400
pan2 0 -260
wait 340
click "workspace 1"
wait 320
gesture-ms 900
holdmove "calendar" -88.8 0 hold
wait 220
accel "place in new column 2" -
drop
wait 480
click "inbox"
wait 700
gesture-ms 360
pan2 0 -260
wait 250
gesture-ms 600
pan2 -180 0
wait 200
pan2 180 0
wait 200
gesture-ms 300
pan2 0 260
wait 30000
quit
''')
    env=dict(os.environ,MAKEPAD_CAPTURE_BMP='1',SUPERAPP_CAPTURE_DIR=str(dest/'frames'),
             SUPERAPP_CAPTURE_SECONDS='15',MAKEPAD_DISPLAY_LINK='0',MAKEPAD_NO_VSYNC='1')
    with RemoteSession(dest/'demo.db',dest,window='480x850',
            extra_args=['--grid','4x3','--e2e',str(script),'--demo-disk','--e2e-out',str(dest)],
            env=env,binary=ROOT/'.context/promo/bin/superapp-capture') as s:
        wait_ready(s);s.shot('ready');start=s.record_gpu()
        seen=len((dest/'app.log').read_text().splitlines());events=[]
        while not (dest/'frames/complete').exists():
            lines=(dest/'app.log').read_text().splitlines()
            for line in lines[seen:]:
                if 'e2e:' in line: events.append({'time':time.monotonic()-start,'action':line})
            seen=len(lines);time.sleep(.01)
        s.finish_gpu();s.shot('last')
        (dest/'cues.json').write_text(json.dumps(events,indent=2)+'\n')
    if 'e2e: FAIL' in (dest/'app.log').read_text():
        raise RuntimeError('Overview choreography failed; inspect app.log')
    print('Recorded complete Overview choreography',flush=True)


if __name__=='__main__':main()
