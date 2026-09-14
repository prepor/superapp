#!/usr/bin/env python3
"""Rehearse the meeting scenario with the app's real gateway and demo data."""
import datetime
import json
import os
import re
import sqlite3
import subprocess
import time
import urllib.parse
import urllib.request
from zoneinfo import ZoneInfo
from capture import OUT, ROOT


def main():
    dest=OUT/'live-agent'; db=dest/'demo.db'
    c=sqlite3.connect(db)
    def grams(text):
        chars=text.casefold()
        return ''.join('g'+''.join(f'{ord(ch):x}x' for ch in word)+' '
                       for size in range(1,min(3,len(chars))+1)
                       for word in sorted({chars[i:i+size] for i in range(len(chars)-size+1)}))
    c.create_function('tg_search_grams',1,grams,deterministic=True)
    c.execute('pragma foreign_keys=off')
    c.execute("update account set mail_enabled=1, calendar_enabled=1,email='launch@demo.invalid',label='Launch demo'")
    for (table,) in c.execute("select name from sqlite_master where type='table' and name like 'workshop_%'").fetchall():
        c.execute(f'DELETE FROM "{table}"')
    c.execute('delete from effect')
    for table in ['agent_call','agent_turn','agent_run','agent_chat']:
        c.execute(f'DELETE FROM "{table}"')
    c.execute('delete from panel where id!=1')
    c.execute('delete from ws_col where ws!=0 or idx!=0')
    c.execute('update workspace set focus=1 where k=0')
    c.execute('update wm set active=0')
    start=datetime.datetime(2026,9,15,16,tzinfo=ZoneInfo('Europe/Berlin'))
    raw=json.loads(c.execute('select raw from calendar_event where id=1').fetchone()[0])
    raw.update(summary='Superapp launch review',description='Review launch readiness. Related mail and the demo Telegram group contain the current status.')
    raw['start']={'dateTime':start.isoformat(),'timeZone':'Europe/Berlin'}
    raw['end']={'dateTime':(start+datetime.timedelta(minutes=30)).isoformat(),'timeZone':'Europe/Berlin'}
    c.execute('update calendar_event set title=?,start=?,end=?,day=?,raw=? where id=1',('Superapp launch review',start.timestamp(),start.timestamp()+1800,'2026-09-15',json.dumps(raw)))
    c.execute("update message set subject='Superapp launch review',topic='Superapp launch review',body='Docs ready for the Superapp launch review. The documentation is complete.',html=NULL where thread=1 or id=1")
    chat=c.execute("select id from tg_peer where name='stelaxis'").fetchone()[0]
    seq=c.execute('select seq from tg_message where chat=? order by date desc limit 1',(chat,)).fetchone()[0]
    c.execute('update tg_message set text=?,date=? where seq=?',('Superapp launch review: Android clip pending. Everything else is ready.',start.timestamp()-86400,seq))
    c.execute("update panel set kind='calendar-event',args='[\"1\"]' where id=1")
    c.commit(); c.close()
    log=(dest/'live.log').open('w')
    env=dict(os.environ,MAKEPAD_NO_FOCUS='1',MAKEPAD_PRESENT_WHEN_OCCLUDED='1')
    env.pop('MAKEPAD_FOCUS',None)
    proc=subprocess.Popen([str(ROOT/'target/debug/superapp'),'--db',str(db),'--window','1440x900','--remote'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,env=env)
    recording=None
    try:
        port=None
        for _ in range(100):
            text=(dest/'live.log').read_text()
            match=re.search(r'listening on 127\.0\.0\.1:(\d+)',text)
            if match: port=match.group(1); break
            if proc.poll() is not None: raise RuntimeError('App exited before remote control started')
            time.sleep(.1)
        if port is None: raise RuntimeError('No control port')
        def call(path,**params):
            with urllib.request.urlopen(f'http://127.0.0.1:{port}/{path}?'+urllib.parse.urlencode(params),timeout=20) as r:
                return r.read()
        time.sleep(2)
        window=subprocess.check_output([str(OUT/'bin/window-id'),str(proc.pid)],text=True).strip()
        recording=subprocess.Popen(['/usr/sbin/screencapture','-x','-o','-v','-V85',f'-l{window}',str(dest/f'live-{time.time_ns()}.mov')])
        call('k',c='KeyA',cmd=1,shift=1)
        time.sleep(.8)
        (dest/'widgets.json').write_bytes(call('snap'))
        call('click',x=560,y=842)
        time.sleep(.25)
        call('k',c='ArrowDown')
        call('k',c='ReturnKey')
        time.sleep(.2)
        prompt='Prep me for this meeting using the related cached mail and Telegram messages. Save and open a short note titled Launch prep. Read existing records and use notes.create. Do not create or edit calendar events or send messages.'
        call('click',x=710,y=795)
        time.sleep(.2)
        call('t',t=prompt)
        time.sleep(1.5)
        (dest/'prompt.png').write_bytes(call('g',raw=1))
        call('k',c='ReturnKey')
        print('Live gateway request submitted from the calendar panel.',flush=True)
        read=sqlite3.connect(f'file:{db}?mode=ro',uri=True)
        for i in range(120):
            row=read.execute('select status,error from agent_run order by id desc limit 1').fetchone()
            if row and row[0] in ('done','failed','stopped'):
                print('Live run:',row[0],row[1] or '',flush=True)
                (dest/'result.png').write_bytes(call('g',raw=1))
                notes=read.execute("select id,title,body from notes_note where title like '%Launch prep%'").fetchall()
                calls=[dict(zip(('tool','status','input','output'),call)) for call in read.execute('select tool,status,input,output from agent_call')]
                (dest/'result.json').write_text(json.dumps({'status':row[0],'error':row[1],'notes':notes,'calls':calls},indent=2)+'\n')
                time.sleep(3)
                break
            if i%10==0: print('Waiting for the live run:',row[0] if row else 'no run row yet',flush=True)
            time.sleep(1)
        else: raise TimeoutError('Live run still pending after 120 seconds')
    finally:
        if recording and recording.poll() is None:
            try: recording.wait(timeout=90)
            except subprocess.TimeoutExpired: recording.terminate(); recording.wait()
        if proc.poll() is None:
            proc.terminate()
            try: proc.wait(timeout=5)
            except subprocess.TimeoutExpired: proc.kill(); proc.wait()
        log.close()


if __name__=='__main__': main()
