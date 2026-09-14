#!/usr/bin/env python3
"""Validate actual source content, framing continuity, and the encoded cut."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import numpy as np
from render_v2 import Film, NativeFrames
from scenes import V2
from capture import OUT


def digest(path): return hashlib.sha256(path.read_bytes()).hexdigest()


def source_report(name):
    f=NativeFrames(name)
    dt=np.diff(f.times)*1000
    return {'native_frames':len(f.paths),'duration':f.times[-1],
            'frame_interval_ms':{'median':float(np.median(dt)),
                                 'p99':float(np.percentile(dt,99)),
                                 'maximum':float(max(dt))},
            'format':f.paths[0].suffix[1:]}


def main():
    p=argparse.ArgumentParser();p.add_argument('--encoded',action='store_true');a=p.parse_args()
    report={}
    for name in ['speed','choreography','phone','android-desktop']:
        report[name]=source_report(name)
    f=NativeFrames('speed')
    ids=[f.index(.1+i/60) for i in range(600)]
    hashes=[digest(f.paths[i]) for i in ids]
    repeats=[i for i in range(1,600) if hashes[i]==hashes[i-1]]
    report['speed'].update(export_frames=600,identical_consecutive_source_images=len(repeats),
                           repeated_at_seconds=[i/60 for i in repeats],playback_speed=1,
                           interpolation=False)
    assert not repeats,'Repeated motion frames in speed section'
    assert max(np.diff(f.times))<=.02,'Source gap exceeds the requested 50–60 fps cadence'
    film=Film(previews=True)
    before=np.asarray(film.render(19-1/60))[224:1030,64:1856]
    after=np.asarray(film.render(19))[224:1030,64:1856]
    report['os_to_ui']={'viewport_pixel_difference':int(np.max(np.abs(before.astype(int)-after.astype(int))))}
    assert np.array_equal(before,after),'The OS / One UI frame jumps'
    report['agents']={}
    for name in ['live-meeting','live-language']:
        result=json.loads((V2/name/'result.json').read_text())
        calls=result['calls']
        assert result['run']['status']=='done'
        assert all(c['status']=='done' for c in calls)
        report['agents'][name]={'status':'done','tools':[c['tool'] for c in calls]}
    sync=json.loads((V2/'sync-result.json').read_text())
    assert sync['same_note_uid'] and not sync['android_device']
    report['sync']=sync
    sessions=[]
    for name in ['speed','choreography','phone','android-desktop','sync-a','sync-b']:
        data=json.loads((V2/name/'session.json').read_text())
        assert data['pid']!=data['frontmost_before'] and data['pid']!=data['frontmost_after']
        assert all(e['frontmost_pid']!=data['pid'] for e in data['events'])
        sessions.append(name)
    report['background_input_verified']=sessions
    if a.encoded:
        path=OUT/'superapp-v2.mp4'
        metadata=json.loads(subprocess.check_output(['ffprobe','-v','error','-show_streams','-show_format','-of','json',str(path)]))
        video=next(s for s in metadata['streams'] if s['codec_type']=='video')
        assert (video['width'],video['height'])==(1920,1080)
        assert video['avg_frame_rate']=='60/1' and int(video['nb_frames'])==5400
        result=subprocess.run(['ffmpeg','-hide_banner','-v','error','-i',str(path),'-f','null','-'],capture_output=True)
        assert result.returncode==0 and not result.stderr,result.stderr.decode()
        report['encoded']={'size':[1920,1080],'fps':60,'frames':5400,
                           'duration':float(metadata['format']['duration']),
                           'decode_errors':0,'bytes':path.stat().st_size}
        # Compare adjacent decoded frames in the motion section, excluding text.
        proc=subprocess.Popen(['ffmpeg','-hide_banner','-v','error','-ss','31','-i',str(path),
            '-t','10','-vf','crop=1792:806:64:224,scale=448:202','-pix_fmt','rgb24',
            '-f','rawvideo','pipe:1'],stdout=subprocess.PIPE)
        prev=None;differences=[];frame_size=448*202*3
        while True:
            data=proc.stdout.read(frame_size)
            if not data:break
            assert len(data)==frame_size
            current=np.frombuffer(data,dtype=np.uint8).astype(np.int16)
            if prev is not None:differences.append(float(np.mean(np.abs(current-prev))))
            prev=current
        assert proc.wait()==0 and len(differences)==599
        report['encoded']['speed_frame_difference']={'minimum':min(differences),
                                                     'median':float(np.median(differences)),
                                                     'identical_frames':sum(x==0 for x in differences)}
        assert all(x>0 for x in differences),'Encoded speed passage contains a repeated image'
        samples=[.3,1.9,3.3,4.9,6.2,7.8,9.3,11,12.4,14.2,18.98,19,19.4,20.9,22.5,
                 23.9,25.2,26.6,28.3,31.05,35,40.5,43.5,49.5,53,57,60,63,67,69.3,72,75,78,81.2,82,83.5,85,89]
        dest=V2/'encoded-review';dest.mkdir(exist_ok=True)
        for t in samples:
            subprocess.run(['ffmpeg','-hide_banner','-v','error','-y','-ss',str(t),'-i',str(path),
                            '-frames:v','1',str(dest/f'{t:g}.png')],check=True)
        report['encoded']['review_stills']=len(samples)
    (V2/'review-report.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(report,indent=2),flush=True)


if __name__=='__main__':main()
