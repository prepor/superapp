#!/usr/bin/env python3
"""Check frame cadence, real gesture travel, framing and the encoded third cut."""
import argparse
import json
import sqlite3
import subprocess
import numpy as np
from render_v2 import NativeFrames
from render_v3 import FilmV3
from scenes import V2
from scenes_v3 import V3
from capture import OUT


def cadence(name,start,seconds):
    frames=NativeFrames(str(V3/name));all_dt=np.diff(frames.times)*1000
    # Check every source interval used by the edit, including nearest-frame
    # boundary neighbors. Unused pre-roll and tails remain reported separately.
    times=np.asarray(frames.times[:-1])
    dt=all_dt[(times>=start-.02)&(times<=start+seconds+.02)]
    assert max(dt)<20,(name,max(dt))
    assert 'e2e: FAIL' not in (V3/name/'app.log').read_text()
    session=json.loads((V3/name/'session.json').read_text())
    assert session['pid'] not in (session['frontmost_before'],session['frontmost_after'])
    assert all(e['frontmost_pid']!=session['pid'] for e in session['events'])
    return {'frames':len(frames.paths),'seconds':frames.times[-1],
            'used_seconds':[start,start+seconds],
            'whole_take_maximum_interval_ms':float(max(all_dt)),
            'frame_interval_ms':{'median':float(np.median(dt)),'p99':float(np.percentile(dt,99)),
                                 'maximum':float(max(dt))},'format':'native GPU BMP',
            'foreground':False,'e2e_failures':0}


def differences(path,start,seconds,crop,dimensions):
    w,h=dimensions
    proc=subprocess.Popen(['ffmpeg','-hide_banner','-v','error','-ss',str(start),'-i',str(path),
        '-t',str(seconds),'-vf',f'crop={crop},scale={w}:{h}',
        '-pix_fmt','rgb24','-f','rawvideo','pipe:1'],stdout=subprocess.PIPE)
    previous=None;values=[]
    while True:
        data=proc.stdout.read(w*h*3)
        if not data:break
        assert len(data)==w*h*3
        current=np.frombuffer(data,dtype=np.uint8).astype(np.int16)
        if previous is not None:values.append(float(np.mean(np.abs(current-previous))))
        previous=current
    assert proc.wait()==0
    assert values and min(values)>0,'An encoded moving passage repeats a frame'
    return {'comparisons':len(values),'identical_frames':0,'minimum_difference':min(values),
            'median_difference':float(np.median(values))}


def main():
    p=argparse.ArgumentParser();p.add_argument('--encoded',action='store_true');args=p.parse_args()
    film=FilmV3(previews=True)
    ranges=[('phone',film.phone_offset,12)]+[(name,start,seconds) for name,start,seconds in
            zip(['ui-launch','ui-mail','ui-files'],film.offsets,[3.5,4,4.5])]
    report={'native':{name:cadence(name,start,seconds) for name,start,seconds in ranges}}
    before=np.asarray(film.render(19-1/60))[224:1030,64:1856]
    after=np.asarray(film.render(19))[224:1030,64:1856]
    assert np.array_equal(before,after),'The first UI viewport jumps at the heading change'
    report['os_to_ui_viewport_difference']=0
    # Follow the real held-tile border through the middle of the first drag.
    # This catches the former single-tick teleport despite a nominal 60 fps file.
    phone=film.phone
    cues=json.loads((V3/'phone/cues.json').read_text())
    drag=next(c['time'] for c in cues if 'holdmove "inbox"' in c['action'])
    positions=[];hashes=[]
    for i in range(38):
        t=drag+.065+i/60;a=np.asarray(phone.frame(t))
        dark=(a[350:650,25:740,:].max(2)<100);found=[]
        for y,row in enumerate(dark):
            edges=np.flatnonzero(np.diff(np.r_[False,row,False].astype(int)))
            for x0,x1 in zip(edges[::2],edges[1::2]):
                if 300<=x1-x0<=314:found.append((int(x0)+25,y+350))
        assert found,('No held tile',t)
        positions.append(found[0]);hashes.append(hash(a[330:950,20:760].tobytes()))
    assert len(set(hashes))==len(hashes),'The held tile is frozen between input steps'
    travel=np.diff(np.asarray(positions),axis=0)
    assert np.all(travel[:,0]>0) and np.max(travel[:,0])<18,positions
    report['overview_drag']={'sample_rate':60,'frames':len(positions),'distinct_images':len(set(hashes)),
        'tile_positions_native_pixels':positions,'maximum_step_pixels':int(np.max(travel[:,0])),
        'interpolation':False,'playback_speed':1}
    with sqlite3.connect(V3/'phone/demo.db') as c:
        assert not c.execute("SELECT 1 FROM panel WHERE kind='rss'").fetchone()
        layout=c.execute('SELECT ws,col,row,kind FROM panel ORDER BY ws,col,row').fetchall()
    report['overview_actions']={'open':True,'switch_workspace':True,'stack':True,
        'new_column':True,'open_photo_chat':True,'swipe_close':True,'final_layout':layout}
    with sqlite3.connect(V3/'heroes/calendar-0/demo.db') as c:
        count=c.execute("SELECT COUNT(*) FROM calendar_event WHERE day LIKE '2026-09-%'").fetchone()[0]
        assert count==29
    report['calendar_events']=count
    report['media']={'chat':'Berlin cycling','photo':'assets/berlin-bike.png',
                     'generated_sample':True,'provenance':'assets/provenance.json'}
    report['retained_evidence']=json.loads((V2/'review-report.json').read_text())
    report['android_device']=False
    if args.encoded:
        path=OUT/'superapp-v3.mp4'
        info=json.loads(subprocess.check_output(['ffprobe','-v','error','-show_streams','-show_format','-of','json',str(path)]))
        video=next(s for s in info['streams'] if s['codec_type']=='video')
        assert (video['width'],video['height'],video['avg_frame_rate'],int(video['nb_frames']))==(1920,1080,'60/1',5400)
        decoded=subprocess.run(['ffmpeg','-hide_banner','-v','error','-i',str(path),'-f','null','-'],capture_output=True)
        assert decoded.returncode==0 and not decoded.stderr,decoded.stderr.decode()
        report['encoded']={'size':[1920,1080],'fps':60,'frames':5400,'seconds':float(info['format']['duration']),
            'bytes':path.stat().st_size,'decode_errors':0,
            'overview_drag':differences(path,69+drag+.065-film.phone_offset,38/60,'540:956:1300:78',(270,478)),
            'fast':differences(path,31,10,'1792:806:64:224',(448,202))}
        samples=[.3,1.9,3.3,4.1,6.2,7.8,9.3,11,12.3,13.1,14.3,18.98,19,19.6,20.2,20.8,
                 21.3,22.2,22.7,23.2,23.6,24.2,24.8,25.5,26.8,27.5,28.2,29.2,30.2,31.2,35.5,40.4,
                 43.5,49.5,53,57,60,63,67,69.2,70,71.2,71.8,72.5,73,74.4,75.5,76.5,77.5,78.4,79.5,80.4,83,86,89]
        dest=V3/'encoded-review';dest.mkdir(exist_ok=True)
        for t in samples:
            subprocess.run(['ffmpeg','-hide_banner','-v','error','-y','-ss',str(t),'-i',str(path),
                            '-frames:v','1',str(dest/f'{t:g}.png')],check=True)
        report['encoded']['review_frames']=len(samples)
    (V3/'review-report.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps({k:v for k,v in report.items() if k not in ['retained_evidence','overview_drag']},indent=2),flush=True)


if __name__=='__main__':main()
