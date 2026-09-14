#!/usr/bin/env python3
"""Record only a scripted demo window, using macOS video capture at real speed."""
import argparse
import json
import os
import subprocess
import time
from capture import OUT, ROOT, launch
from remote_session import RemoteSession


def record_choreography():
    dest = OUT/'native/choreography'
    dest.mkdir(parents=True, exist_ok=True)
    body = (OUT/'captures/choreography/script.txt').read_text()
    prefix = body.split('key cmd+,', 1)[0]
    script = dest/'controlled.txt'
    script.write_text(prefix+'shot ready\nwait 60000\nquit\n')
    ready = dest/'ready.png'
    if ready.exists():
        ready.unlink()
    with RemoteSession(dest/f'take-{time.time_ns()}.db', dest, extra_args=[
            '--e2e', str(script), '--demo-disk', '--e2e-out', str(dest)]) as session:
        deadline = time.monotonic()+30
        while not ready.exists():
            if time.monotonic() > deadline:
                raise TimeoutError('Workspace did not reach its ready marker')
            time.sleep(.1)
        session.record(13)
        start = time.monotonic()
        cues = []
        for at, code, label in [(1.5, 'Comma', '⌘,'), (3, 'Period', '⌘.'),
                                (4.5, 'RBracket', '⌘]'), (6, 'KeyT', '⌘T'),
                                (8, 'Key2', '⌘2'), (10, 'Key1', '⌘1')]:
            time.sleep(max(0, at-(time.monotonic()-start)))
            session.call('k', c=code, cmd=1)
            cues.append({'time': time.monotonic()-start, 'key': label})
        (dest/'cues.json').write_text(json.dumps(cues, indent=2)+'\n')
    print(dest/'recording.mov', flush=True)


def speed_script():
    text = 'wait 1000\n'
    subjects=['Q3 infra','Sat hike','superapp panel model','that airport book']
    for i in range(32):
        text += f'cmdclick "{subjects[i%4]}"\nwait 180\nkey cmd+left\nwait 150\n'
    text += launch('notes')+'click "new note"\nwait 350\nclick "editor"\nwait 150\npaste "Launch prep\\n\\nOne workspace.\\nRoom to think."\nwait 300\nkey esc\nwait 300\n'
    text += 'shot ready\nwait 2500\n'
    for _ in range(34):
        text += 'key cmd+shift+left\nwait 110\n'
    text += 'wait 650\n'
    for _ in range(34):
        text += 'key cmd+right\nwait 90\n'
    return text+'wait 5000\nquit\n'


def record(name):
    if name == 'choreography':
        record_choreography()
        return
    dest=OUT/'native'/name; dest.mkdir(parents=True,exist_ok=True)
    text=speed_script(); duration=14
    script=dest/'script.txt'; script.write_text(text)
    marker=dest/'ready.png'
    if marker.exists(): marker.unlink()
    log=(dest/'app.log').open('w')
    env=dict(os.environ,MAKEPAD_NO_FOCUS='1',MAKEPAD_PRESENT_WHEN_OCCLUDED='1')
    env.pop('MAKEPAD_FOCUS',None)
    proc=subprocess.Popen([str(ROOT/'target/debug/superapp'),'--e2e',str(script),'--demo-disk','--window','1440x900','--e2e-out',str(dest),'--remote'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,env=env)
    try:
        deadline=time.monotonic()+100
        while not marker.exists():
            if proc.poll() is not None: raise RuntimeError((dest/'app.log').read_text()[-3000:])
            if time.monotonic()>deadline: raise TimeoutError('Demo did not reach its capture marker')
            time.sleep(.1)
        window=subprocess.check_output([str(OUT/'bin/window-id'),str(proc.pid)],text=True).strip()
        print(f'Recording {name}: window {window}',flush=True)
        if (dest/'recording.mov').exists():
            (dest/'recording.mov').replace(dest/f'recording-{time.time_ns()}.mov')
        subprocess.run(['/usr/sbin/screencapture','-x','-o','-v',f'-V{duration}',f'-l{window}',str(dest/'recording.mov')],check=True)
        print(f'Saved {dest / "recording.mov"}',flush=True)
    finally:
        if proc.poll() is None:
            proc.terminate()
            try: proc.wait(timeout=5)
            except subprocess.TimeoutExpired: proc.kill(); proc.wait()
        log.close()


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__); p.add_argument('scene',choices=['speed','choreography']); a=p.parse_args()
    record(a.scene)
