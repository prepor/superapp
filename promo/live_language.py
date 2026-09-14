#!/usr/bin/env python3
"""Run RSS → German practice through the real agent in a dedicated demo store."""
import io
import argparse
import json
import sqlite3
import time
from PIL import Image
from capture import OUT, ROOT
from remote_session import RemoteSession


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--bike',action='store_true')
    bike=parser.parse_args().bike
    dest = OUT/('v2/live-language' if bike else 'live-language')
    dest.mkdir(parents=True, exist_ok=True)
    db = dest/'demo.db'
    if db.exists():
        raise RuntimeError('This take already has a database; archive it before another take')
    source = sqlite3.connect(f'file:{OUT / "live-agent/demo.db"}?mode=ro', uri=True)
    c = sqlite3.connect(db)
    source.backup(c)
    source.close()
    for table in ('agent_call', 'agent_turn', 'agent_run', 'agent_chat', 'effect'):
        c.execute(f'DELETE FROM "{table}"')
    c.executescript('''
        DELETE FROM panel WHERE id != 1;
        DELETE FROM ws_col WHERE ws != 0 OR idx != 0;
        UPDATE workspace SET focus = 1 WHERE k = 0;
        UPDATE wm SET active = 0;
        UPDATE account SET mail_enabled = 0, calendar_enabled = 0;
        UPDATE calendar_source SET active = 0, error = '';
        UPDATE fluent_learner SET native = 'English', target = 'German', level = 'A2';
    ''')
    c.execute('UPDATE panel SET kind = ?, args = ? WHERE id = 1', ('rss-article', '["2"]'))
    if bike:
        from scenes import BIKE_TITLE, BIKE_HTML
        c.execute('UPDATE rss_article SET title=?,html=?,raw=NULL WHERE id=2',(BIKE_TITLE,BIKE_HTML))
        c.execute('UPDATE rss_feed SET checked=1e20,requested=completed')
    previous = c.execute('SELECT COALESCE(MAX(id), 0) FROM fluent_lesson').fetchone()[0]
    c.commit()
    c.close()
    with RemoteSession(db, dest, binary=ROOT/'target/release/superapp') as s:
        s.record(85)
        s.shot('article')
        s.call('k', c='KeyA', cmd=1, shift=1)
        time.sleep(.8)
        screenshot = s.call('g', raw=1)
        im = Image.open(io.BytesIO(screenshot)).convert('RGB')
        scale = im.width/1440
        # The focused chat is the only solid black header after opening it.
        pixels = im.load()
        candidates = [x for x in range(im.width-100)
                      if all(max(pixels[x+i, round(20*scale)]) < 45 for i in range(100))]
        left = min(candidates)/scale
        s.call('click', x=left+64, y=842)
        s.call('k', c='ArrowDown')
        s.call('k', c='ReturnKey')
        prompt = ('Turn this article into a short A2 German lesson. Read the article from '
                  'its attached panel, call fluent.due, then save five exercises with '
                  'fluent.author: warmup, review, new, set_piece, cooldown. Use the article\'s '
                  'ideas about reading and attention. Title it Zeit zum Lesen. Use English '
                  'instructions and two new German vocabulary cards. Save it for today. '
                  'Only author this lesson; do not grade old lessons or contact anyone.')
        if bike:
            prompt=('Turn the bike-rental advice in this article into German practice. Read '
                    'the attached RSS article, call fluent.due, then use fluent.author to '
                    'save a five-exercise lesson titled Ein Fahrrad, bitte. Use English '
                    'instructions at A2 level. The first exercise must be a closed mcq: '
                    'How do you say “I’d like to rent a bike”? Its correct choice must be '
                    '“Ich möchte ein Fahrrad mieten.” Include two plausible wrong choices. '
                    'Use the article’s bike rental, helmet and return-time context in the '
                    'remaining exercises. Include vocabulary cards for das Fahrrad and '
                    'mieten. Save for today. Only author this lesson; do not grade old '
                    'lessons or contact anyone.')
        s.call('click', x=left+200, y=795)
        s.call('t', t=prompt)
        time.sleep(.6)
        s.shot('prompt')
        s.call('k', c='ReturnKey')
        print('Language lesson requested from the RSS panel.', flush=True)
        read = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
        read.row_factory = sqlite3.Row
        for i in range(150):
            run = read.execute('SELECT status, error FROM agent_run ORDER BY id DESC LIMIT 1').fetchone()
            if run and run['status'] in ('done', 'failed', 'stopped'):
                s.shot('result')
                calls = [dict(row) for row in read.execute('SELECT tool, status, input, output FROM agent_call')]
                lessons = [dict(row) for row in read.execute('SELECT * FROM fluent_lesson WHERE id > ?', (previous,))]
                exercises = [dict(row) for row in read.execute('SELECT * FROM fluent_exercise WHERE lesson > ?', (previous,))]
                (dest/'result.json').write_text(json.dumps({
                    'run': dict(run), 'calls': calls, 'lessons': lessons,
                    'exercises': exercises}, indent=2)+'\n')
                print('Live language run:', run['status'], 'lessons:', len(lessons),
                      'exercises:', len(exercises), flush=True)
                break
            if i%15 == 0:
                print('Language run:', run['status'] if run else 'starting', flush=True)
            time.sleep(1)
        else:
            raise TimeoutError('Language run did not complete in 150 seconds')
        read.close()


if __name__ == '__main__':
    main()
