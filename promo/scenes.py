"""Prepared, fictional data and deliberate framing for the revised film."""
import json
import sqlite3
from pathlib import Path
from capture import OUT

V2 = OUT/'v2'
BASE = V2/'base.db'
BIKE_TITLE = 'Berlin by bike'
BIKE_HTML = '''<h2>A weekend on two wheels</h2>
<p>Start by the Spree. Ride through Tiergarten. Stop for a coffee.</p>
<h2>At the bike shop</h2>
<p>The first thing you need to say:</p>
<blockquote>I'd like to rent a bike.</blockquote>
<p>Ask for a helmet, check the brakes, and find out when the bike is due back.</p>'''


def copy_database(path, source=BASE):
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        raise RuntimeError(f'Archive this take before replacing its database: {path}')
    reader = sqlite3.connect(f'file:{source}?mode=ro', uri=True)
    c = sqlite3.connect(path)
    reader.backup(c)
    reader.close()
    c.execute('PRAGMA foreign_keys=OFF')
    return c


def prepare(path, columns, second=None, focus=1, source=BASE):
    c = copy_database(path, source)
    c.executescript('DELETE FROM panel; DELETE FROM ws_col; UPDATE workspace SET focus=NULL; UPDATE wm SET active=0; DELETE FROM effect;')
    c.execute("UPDATE account SET status='ok',mail_enabled=0,calendar_enabled=1")
    c.execute("UPDATE calendar_source SET active=1,error='',checked=1e20")
    c.execute('UPDATE rss_feed SET checked=1e20,requested=completed')
    if source == BASE:
        c.execute('INSERT OR REPLACE INTO notes_note(id,title,body) VALUES(1,?,?)',
                  ('Launch checklist', 'Launch checklist\n\n[ ] Finish the walkthrough\n[ ] Update the docs\n[ ] Test on Android\n[ ] Share superapp'))
        c.execute('INSERT OR REPLACE INTO notes_note(id,title,body) VALUES(2,?,?)',
                  ('Today', 'Today\n\nDesign. Build. Read.\n\nOne place for everything.'))
        c.execute('UPDATE rss_article SET title=?,html=?,raw=NULL WHERE id=2', (BIKE_TITLE,BIKE_HTML))
        c.execute('UPDATE rss_feed SET checked=1e20, requested=completed')
        c.execute('UPDATE notes_note SET created=1789405200,modified=1789405200')
        c.execute('UPDATE fluent_learner SET native=?,target=?,level=?', ('English','German','A2'))
        c.execute('UPDATE workshop_workspace SET activity=1789405200,refresh_requested=0')
        for i,label in enumerate(['berlin','kyoto','lisbon','osaka','vienna','seoul'],3):
            c.execute('''INSERT OR IGNORE INTO workshop_workspace
                (id,project_id,label,path,branch,base_ref,status,activity,refresh_requested)
                VALUES(?,1,?,?,?,'origin/main','ready',?,0)''',
                (i,label,'/sample/workspaces/'+label,'workshop/'+label,1789405200-i*600))
    slot = 0
    for ws, layout in enumerate([columns]+([second] if second else [])):
        first = slot+1
        for col, panels in enumerate(layout):
            c.execute('INSERT INTO ws_col(ws,idx) VALUES(?,?)', (ws,col))
            for row, (kind,args) in enumerate(panels):
                slot += 1
                c.execute('INSERT INTO panel(id,ws,col,row,kind,args) VALUES(?,?,?,?,?,?)',
                          (slot,ws,col,row,kind,json.dumps(args)))
        c.execute('UPDATE workspace SET focus=? WHERE k=?', (focus if ws==0 else first,ws))
    c.commit()
    c.close()


HEROES = {
    'mail': [('inbox',[]), ('message',['1'])],
    'files': [('files',['~/Downloads']), ('file',['~/Downloads/report-q3.pdf'])],
    'calendar': [('calendar-month',['2026-09-01','']), ('calendar-event',['1'])],
    'workshop': [('workshop_workspaces',[]), ('workshop_review',['1'])],
    'terminal': [('terminal',[])],
    'rss': [('rss',[]), ('rss-article',['2'])],
    'notes': [('notes',[]), ('editor',['note','1'])],
    'agents': [('chat',['1'])],
    'telegram': [('chats',[]), ('telegram-chat',['10'])],
    'fluent': [('review',[]), ('lesson',['100'])],
}

CHOREOGRAPHY = [[('inbox',[])], [('chats',[])],
                [('rss',[]),('notes',[])], [('files',['~/Downloads'])]]
SECOND = [[('workshop_workspaces',[])], [('files',['~/Downloads'])], [('inbox',[])]]
