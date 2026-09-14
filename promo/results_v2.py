#!/usr/bin/env python3
"""Record fully framed results and the three actual database-backed lists."""
import argparse
import json
from capture import ROOT
from scenes import V2, BASE, prepare
from remote_session import RemoteSession
from shoot_v2 import wait_file


def take(name, panel, source=BASE, window='600x620', after=''):
    dest=V2/'details'/name
    prepare(dest/'demo.db',[[panel]],source=source)
    script=dest/'take.txt'
    script.write_text('wait 1000\nshot ready\n'+after+'wait 30000\nquit\n')
    with RemoteSession(dest/'demo.db',dest,window=window,
            extra_args=['--grid','6x6','--e2e',str(script),'--demo-disk','--e2e-out',str(dest)],
            binary=ROOT/'target/release/superapp'):
        wait_file(dest/('answer.png' if after else 'ready.png'))
    print('Result view',name,flush=True)


def main():
    p=argparse.ArgumentParser();p.add_argument('what',choices=['lists','language','meeting','agent'])
    a=p.parse_args()
    if a.what=='lists':
        for name,kind in [('mail','inbox'),('telegram','chats'),('workshop','workshop_workspaces')]:
            take('db-'+name,(kind,[]),window='430x440')
    elif a.what=='language':
        source=V2/'live-language/demo.db'
        result=json.loads((source.parent/'result.json').read_text())
        take('bike',('rss-article',['2']),source,window='540x650')
        answer=json.loads(result['exercises'][0]['accepted'])[0]
        take('lesson',('lesson',[str(result['lessons'][-1]['id'])]),source,window='650x650',
             after='wait 1000\nclick '+json.dumps(answer,ensure_ascii=False)+'\nwait 700\nshot answer\n')
    elif a.what=='agent':
        take('agent',('chat',['1']),V2/'live-language/demo.db',window='720x480')
    else:
        source=V2/'live-meeting/demo.db'
        result=json.loads((source.parent/'result.json').read_text())
        take('meeting',('calendar-event',['1']),source,window='540x480')
        take('launch-note',('editor',['note',str(result['notes'][-1]['id'])]),source,window='720x570')
        take('meeting-agent',('chat',['1']),source,window='720x570')


if __name__=='__main__': main()
