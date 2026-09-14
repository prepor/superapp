#!/usr/bin/env python3
"""A short, real agent run using only the prepared launch records."""
import io
import json
import sqlite3
import time
from PIL import Image
from capture import OUT, ROOT
from scenes import V2, prepare
from remote_session import RemoteSession


def main():
    dest = V2/'live-meeting'
    db = dest/'demo.db'
    prepare(db, [[('calendar-event',['1'])]], source=OUT/'live-agent/demo.db')
    c = sqlite3.connect(db)
    c.executescript('''
        DELETE FROM agent_call; DELETE FROM agent_turn;
        DELETE FROM agent_run; DELETE FROM agent_chat; DELETE FROM notes_note;
        UPDATE account SET status='ok',mail_enabled=0,calendar_enabled=1;
        UPDATE calendar_source SET active=1,error='',checked=1e20;
        UPDATE calendar_sync SET checked=1e20,requested=completed,error='',start=1700000000,end=1900000000;
        UPDATE rss_feed SET checked=1e20,requested=completed;
        UPDATE message SET subject='Superapp launch: docs ready',topic='Superapp launch',
            body='The Superapp launch documentation is complete. All setup and Android guides are ready.',
            html=NULL WHERE id=1 OR thread=1;
    ''')
    c.commit(); c.close()
    with RemoteSession(db,dest,binary=ROOT/'target/release/superapp') as s:
        s.shot('event')
        s.call('k',c='KeyA',cmd=1,shift=1)
        time.sleep(.7)
        im=Image.open(io.BytesIO(s.call('g',raw=1))).convert('RGB')
        scale=im.width/1440
        px=im.load()
        left=next(x for x in range(im.width-100)
                  if all(max(px[x+i,round(20*scale)])<45 for i in range(100)))/scale
        s.call('click',x=left+64,y=842)
        s.call('k',c='ArrowDown'); s.call('k',c='ReturnKey')
        prompt=('Prep me for this meeting. Read its calendar event, the cached email and '
                'Telegram updates about the Superapp launch. Save a note called Launch prep '
                'with the meeting time, documentation status, Android clip status, and three '
                'useful questions for the meeting. Keep the note under 100 words. Use cached '
                'message text; skip attachments. Use notes.create. Do not send messages or '
                'change the calendar.')
        s.call('click',x=left+200,y=795); s.call('t',t=prompt)
        time.sleep(.3); s.shot('prompt'); s.call('k',c='ReturnKey')
        c=sqlite3.connect(f'file:{db}?mode=ro',uri=True); c.row_factory=sqlite3.Row
        for i in range(150):
            run=c.execute('SELECT status,error FROM agent_run ORDER BY id DESC LIMIT 1').fetchone()
            if run and run['status'] in ('done','failed','stopped'):
                s.shot('result')
                result={'run':dict(run),
                    'calls':[dict(r) for r in c.execute('SELECT tool,status,input,output FROM agent_call')],
                    'notes':[dict(r) for r in c.execute('SELECT id,title,body FROM notes_note')]}
                (dest/'result.json').write_text(json.dumps(result,indent=2)+'\n')
                print('Meeting run:',dict(run),flush=True)
                break
            if i%15==0: print('Waiting for meeting note',flush=True)
            time.sleep(1)
        else: raise TimeoutError('Meeting agent did not finish')
        c.close()


if __name__=='__main__': main()
