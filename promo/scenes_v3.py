"""Fictional, populated calendar and cached Telegram photo for promo takes."""
from datetime import datetime, timedelta, timezone
import hashlib
import json
import shutil
import sqlite3
from capture import OUT
from scenes import prepare

V3 = OUT/'v3'
PHOTO_REF = 'tg:promo-berlin-bike-v3'


def search_grams(text):
    folded=text.casefold()
    return ''.join('g'+''.join(f'{ord(ch):x}x' for ch in gram)+' '
        for size in range(1, min(3,len(folded))+1)
        for gram in sorted({folded[i:i+size] for i in range(len(folded)-size+1)}))


def prepare_v3(path, columns, **kwargs):
    prepare(path, columns, **kwargs)
    c = sqlite3.connect(path)
    # Match the app's FTS trigger input when preparing the isolated fixture.
    c.create_function('tg_search_grams', 1, search_grams)
    now=datetime.now(timezone.utc).timestamp()
    c.execute('UPDATE calendar_sync SET start=1785535200,end=1819756800,requested=completed,checked=?,error=\'\'',(now,))
    c.execute('UPDATE calendar_source SET checked=?',(now,))
    c.execute('DELETE FROM calendar_event')
    entries = [
        (1,'Design review',14,45),(2,'Planning',10,60),(3,'Studio all-hands',11,60),
        (4,'German practice',18,45),(5,'Bike ride',9,120),(7,'Weekly planning',10,45),
        (8,'Design review',14,45),(9,'Coffee with Nora',10,45),(10,'Build session',14,90),
        (11,'German practice',18,45),(12,'Museum afternoon',13,120),
        (14,'Weekly planning',9,45),(14,'Design review',11,45),(14,'German practice',18,45),
        (15,'Launch review',16,30),(15,'Coffee with Vera',10,45),(16,'Studio all-hands',11,60),
        (17,'Android testing',10,90),(18,'German practice',18,45),(19,'Bike ride',9,120),
        (21,'Weekly planning',10,45),(22,'Design review',14,45),(23,'Build session',14,90),
        (24,'Coffee with Nora',10,45),(25,'German practice',18,45),(26,'Weekend away',9,480),
        (28,'Weekly planning',10,45),(29,'Design review',14,45),(30,'Launch retrospective',11,60),
    ]
    for i,(day,title,hour,minutes) in enumerate(entries,101):
        start=datetime(2026,9,day,hour,tzinfo=timezone(timedelta(hours=2)))
        end=start+timedelta(minutes=minutes)
        guests=[{'email':'vera@studio.example','displayName':'Vera','responseStatus':'accepted'},
                {'email':'nora@studio.example','displayName':'Nora','responseStatus':'accepted'}]
        raw={'id':f'promo-{i}','summary':title,'status':'confirmed',
             'start':{'dateTime':start.isoformat(),'timeZone':'Europe/Berlin'},
             'end':{'dateTime':end.isoformat(),'timeZone':'Europe/Berlin'},
             'organizer':{'self':True,'email':'me@demo.invalid'},'attendees':guests,
             'description':'<p>Review the latest work and plan the next steps.</p>'}
        c.execute('''INSERT INTO calendar_event(id,source,remote,title,start,end,day,all_day,guests,response,raw)
                     VALUES(?,1,?,?,?,?,?,0,?,'accepted',?)''',
                  (i,raw['id'],title,start.timestamp(),end.timestamp(),start.date().isoformat(),
                   'Vera, Nora',json.dumps(raw)))
    # A real attachment path consumed by the app's ordinary cached-photo widget.
    c.execute('DELETE FROM tg_message WHERE chat=11')
    rows=[(901,2,'Saturday route: along the Spree, then coffee?',None,None),
          (902,1,'Perfect. I found a bike.',None,None),
          (903,2,'A little preview of the route.', 'photo',PHOTO_REF),
          (904,1,'See you by the river at nine.',None,None)]
    for i,sender,body,media,reference in rows:
        c.execute('''INSERT INTO tg_message(id,chat,sender,date,text,out,state,media,media_ref,media_w,media_h)
            VALUES(?,11,?,1789376400+?, ?,?,'read',?,?,1448,1086)''',
            (i,sender,i,body,int(sender==1),media,reference))
    c.execute("UPDATE tg_peer SET name='Berlin cycling' WHERE id=11")
    c.execute('UPDATE tg_chat SET last_read=0,unread=4,typing=NULL,draft=NULL WHERE peer=11')
    c.commit();c.close()
    blobs=path.parent/'blobs';blobs.mkdir(exist_ok=True)
    shutil.copy2(V3/'assets/berlin-bike.png',blobs/hashlib.sha256(PHOTO_REF.encode()).hexdigest())
