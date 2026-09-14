#!/usr/bin/env python3
"""Render the 90-second first cut from app captures and an original score."""
import argparse
from functools import lru_cache
import json
import math
from pathlib import Path
import subprocess
from PIL import Image, ImageDraw, ImageFont, ImageOps
from capture import OUT, ROOT
from score import compose

CHAPTERS = [(0,'Apps'),(12,'A userspace OS'),(16,'One UI'),(28,'Fast'),
            (38,'SQLite'),(44,'Agents with context'),(58,'Language practice'),
            (66,'Android'),(78,'Sync'),(86,'Open source')]
BG=(14,14,16)
WHITE=(246,245,241)
MUTED=(151,151,158)


def ease(x):
    x=max(0,min(1,x)); return 1-(1-x)**4


@lru_cache(maxsize=48)
def font(size,bold=False):
    path='/System/Library/Fonts/Supplemental/Arial Bold.ttf' if bold else '/System/Library/Fonts/SFNS.ttf'
    return ImageFont.truetype(path,size)


@lru_cache(maxsize=72)
def photo(path):
    return Image.open(path).convert('RGB')


def asset(name):
    found=list((OUT/'captures').glob(f'*/{name}.png'))
    if not found: raise FileNotFoundError(f'Missing capture {name}; run promo/capture.py')
    return str(found[0])


@lru_cache(maxsize=24)
def live_crop(path, box):
    im = photo(str(OUT/path))
    scale = im.width/1440
    return im.crop(tuple(round(v*scale) for v in box))


@lru_cache(maxsize=48)
def crop(name):
    im=photo(asset(name))
    # These are crops of actual panels, not reconstructed UI.
    boxes={
        'mail-list':(8,8,477,892),'mail-thread':(485,8,955,446),
        'files-list':(485,8,955,892),'files-downloads':(485,8,955,892),
        'files-card':(950,8,1432,892),
        'calendar-list':(485,8,955,892),'calendar-month':(485,8,1320,892),
        'calendar-day':(8,8,955,892),
        'workshop-workspace':(450,8,1432,892),'workshop-review':(470,8,1432,892),
        'terminal-empty':(485,8,1195,892),'terminal-output':(485,8,1195,892),
        'rss-list':(485,8,955,892),'rss-article':(840,8,1432,892),
        'telegram-list':(485,8,955,892),'telegram-chat':(960,8,1432,892),
        'fluent-desk':(485,8,955,892),'fluent-exercise':(840,8,1432,892),
        'fluent-answer':(840,8,1432,892),
        'agents-context':(485,8,955,892),'agents-tool':(960,8,1432,892),
        'notes-prep':(720,8,1432,892),'notes-desktop-edit':(720,8,1432,892),
        'notes-phone-edit':(720,8,1432,892),
    }
    return im.crop(boxes.get(name,(0,0,im.width,im.height)))


class Footage:
    def __init__(self,path,fps,width=1440):
        self.files=[]; self.fps=fps
        if not path.exists(): return
        dest=OUT/'decoded'/f'{path.parent.name}-{fps}-{width}'; dest.mkdir(parents=True,exist_ok=True)
        signature=f'{path}:{path.stat().st_size}:{path.stat().st_mtime_ns}'
        if not (dest/'complete').exists() or (dest/'complete').read_text()!=signature:
            for old in dest.glob('*.jpg'): old.unlink()
            subprocess.run(['ffmpeg','-hide_banner','-loglevel','error','-y','-i',str(path),
                            '-vf',f'fps={fps},scale={width}:-2','-q:v','2',str(dest/'%05d.jpg')],check=True)
            (dest/'complete').write_text(signature)
        self.files=sorted(dest.glob('*.jpg'))

    def frame(self,t):
        if not self.files: return None
        index=max(0,min(len(self.files)-1,int(t*self.fps)))
        return photo(str(self.files[index]))


class Film:
    def __init__(self,width,fps,url):
        self.width=width; self.height=width*9//16; self.s=width/1280
        self.url=url
        self.speed=Footage(OUT/'native/speed/recording.mov',fps)
        self.android=Footage(OUT/'native/android/recording.mp4',fps,800)
        self.choreo=Footage(OUT/'native/choreography/recording.mov',fps)
        cues=OUT/'native/choreography/cues.json'
        self.cues=json.loads(cues.read_text()) if cues.exists() else []
        self.lesson=Footage(OUT/'native/agent-lesson/recording.mov',fps)
        meeting_path=OUT/'live-agent/result.json'
        meeting=json.loads(meeting_path.read_text()) if meeting_path.exists() else {}
        self.meeting=(meeting.get('status')=='done' and bool(meeting.get('notes'))
                      and any(c['tool']=='notes.create' and c['status']=='done' for c in meeting.get('calls',[]))
                      and (OUT/'native/agent-note/note.png').exists())
        language_path=OUT/'live-language/result.json'
        language=json.loads(language_path.read_text()) if language_path.exists() else {}
        self.language=(language.get('run',{}).get('status')=='done'
                       and bool(language.get('lessons')) and bool(language.get('exercises'))
                       and any(c['tool']=='fluent.author' and c['status']=='done' for c in language.get('calls',[]))
                       and bool(self.lesson.files))
        self.sync_a=Footage(OUT/'native/sync-a/recording.mov',fps)
        self.sync_b=Footage(OUT/'native/sync-b/recording.mov',fps,800)
        sync_path=OUT/'native/sync-result.json'
        sync=json.loads(sync_path.read_text()) if sync_path.exists() else {}
        self.sync=bool(self.sync_a.files and self.sync_b.files
                       and sync.get('same_note_uid') and sync.get('peer_counts')==[2,2])
        self.frames=sorted((OUT/'captures/choreography/frames').glob('*.png'))

    def text(self,im,words,x,y,size=48,color=WHITE,bold=True,center=False):
        d=ImageDraw.Draw(im); f=font(round(size*self.s),bold)
        if center:
            box=d.textbbox((0,0),words,font=f); x-= (box[2]-box[0])/self.s/2
        d.text((round(x*self.s),round(y*self.s)),words,font=f,fill=color,stroke_width=0)

    def card(self,im,source,x,y,w,h,cover=False,zoom=1):
        rect=tuple(round(v*self.s) for v in (x,y,w,h)); px,py,pw,ph=rect
        if cover:
            pic=ImageOps.fit(source,(max(1,round(pw*zoom)),max(1,round(ph*zoom))),Image.Resampling.LANCZOS,centering=(.5,0))
            if zoom!=1:
                left=(pic.width-pw)//2; top=(pic.height-ph)//2
                pic=pic.crop((left,top,left+pw,top+ph))
        else:
            pic=ImageOps.contain(source,(pw,ph),Image.Resampling.LANCZOS)
            px+=(pw-pic.width)//2; py+=(ph-pic.height)//2; pw,ph=pic.size
        ImageDraw.Draw(im).rectangle((px-1,py-1,px+pw,py+ph),fill=(75,75,79))
        im.paste(pic,(px,py))

    def pending(self,im,label):
        self.text(im,label,36,685,13,MUTED,False)

    def render(self,t):
        im=Image.new('RGB',(self.width,self.height),BG)
        if t<12:
            cuts=[(0,'mail-list'),(.5,'mail-thread'),(1,'files-list'),(1.5,'files-card'),
                  (2,'calendar-list'),(2.5,'calendar-month'),(3,'workshop-workspace'),(3.5,'workshop-review'),
                  (4,'terminal-empty'),(4.5,'terminal-output'),(5,'rss-list'),(5.5,'rss-article'),
                  (6,'notes-prep'),(6.5,'notes-desktop-edit'),(7,'agents-context'),(7.5,'agents-tool'),
                  (8,'telegram-list'),(9,'telegram-chat'),(10,'fluent-desk'),(11,'fluent-answer')]
            start,name=next((a,n) for a,n in reversed(cuts) if a<=t)
            u=t-start
            x=80+65*(1-ease(u/.22))
            self.card(im,crop(name),x,52,1120,620,True,1+.025*u)
        elif t<16:
            u=t-12
            self.card(im,photo(asset('workspace')),70,245+70*(1-ease(u/.8)),1140,580)
            self.text(im,'superapp',640,35,100,center=True)
            self.text(im,'A userspace OS.',640,151,35,MUTED,False,True)
        elif t<28:
            u=t-16
            self.text(im,'One UI. One way to work.',640,42,49,center=True)
            pic=self.choreo.frame(u)
            if pic is None:
                if u<3:
                    pic=photo(asset('mail-thread' if u<1.5 else 'files-card'))
                else:
                    index=min(len(self.frames)-1,int((u-3)/9*len(self.frames)))
                    pic=photo(str(self.frames[max(index,0)]))
            self.card(im,pic,42,146,1196,524)
            cue=next((c for c in reversed(self.cues) if c['time']<=u),None)
            if cue and u-cue['time']<1.25:
                self.text(im,cue['key'],1185,47,32,MUTED,False,True)
        elif t<38:
            u=t-28
            self.text(im,'Fast.',55,22,92)
            self.text(im,'Makepad + shaders.',930,77,25,MUTED,False)
            # Right aligned label, with no frame interpolation or speed change.
            pic=self.speed.frame(u+1.1)
            if pic is not None:
                self.card(im,pic,30,153,1220,540)
            else:
                self.card(im,photo(asset('workspace')),30,153,1220,540)
                self.pending(im,'Native speed capture pending')
        elif t<44:
            u=t-38
            self.text(im,'One shared database.',640,42,53,center=True)
            d=ImageDraw.Draw(im)
            for i,name in enumerate(['mail-thread','calendar-month','notes-prep']):
                x=80+i*410
                self.card(im,crop(name),x,180+35*(1-ease((u-i*.12)/.6)),300,260,True)
                sx=(x+150)*self.s; sy=455*self.s; ex=640*self.s; ey=545*self.s
                p=ease((u-.7)/1)
                d.line((sx,sy,sx+(ex-sx)*p,sy+(ey-sy)*p),fill=(103,103,112),width=max(1,round(2*self.s)))
            d.rounded_rectangle(tuple(round(v*self.s) for v in (498,528,782,635)),radius=round(15*self.s),fill=WHITE)
            self.text(im,'SQLite',640,548,50,BG,True,True)
        elif t<58:
            u=t-44
            if u<2:
                self.text(im,'Agents.',78,136,114)
                self.text(im,'With context.',78,266,95,MUTED)
            elif u<5:
                self.text(im,'“Prep me for this meeting.”',640,45,43,center=True)
                if self.meeting:
                    self.card(im,live_crop('live-agent/prompt.png',(8,8,473,445)),60,173,520,460,True)
                    self.card(im,live_crop('live-agent/prompt.png',(494,685,950,826)),653,259,563,180,True)
                    self.text(im,'⌘⇧A',935,475,34,MUTED,False,True)
                else:
                    self.card(im,photo(asset('agents-context')),75,150,1130,505)
            elif u<9:
                self.text(im,'Across your apps.',640,44,51,center=True)
                if self.meeting:
                    self.card(im,live_crop('live-agent/result.png',(497,47,941,367)),245,163,790,445,True)
                    self.text(im,'Calendar · Mail · Telegram',640,630,25,MUTED,False,True)
                else:
                    for i,name in enumerate(['calendar-day','mail-thread','telegram-chat']):
                        self.card(im,crop(name),45+i*418,163,390,470,True)
            else:
                self.text(im,'Ready for the meeting.',640,44,51,center=True)
                note=live_crop('native/agent-note/note.png',(8,8,713,530)) if self.meeting else crop('notes-prep')
                self.card(im,note,260,155,760,485,True)
            if not self.meeting: self.pending(im,'Agent scenario · live run to be recorded')
        elif t<66:
            u=t-58
            self.text(im,'“Turn this into German practice.”',640,44,43,center=True)
            article=live_crop('live-language/article.png',(8,8,594,610)) if self.language else crop('rss-article')
            self.card(im,article,54,156,450,490,True,1+.003*u)
            self.text(im,'→',576,355,70,MUTED)
            lesson=self.lesson.frame(max(0,u-1))
            if lesson is not None: lesson=lesson.crop((8,8,594,610))
            else: lesson=crop('fluent-exercise' if u<5 else 'fluent-answer')
            self.card(im,lesson,707-80*(1-ease(u/.7)),156,515,490,True)
            if not self.language: self.pending(im,'Agent scenario · live run to be recorded')
        elif t<78:
            u=t-66
            self.text(im,'Android, too.',63,32,70)
            self.card(im,photo(asset('workspace')),30,210,745,470)
            pic=self.android.frame(u)
            if pic is None: pic=photo(asset('phone-overview' if 2<u<9 else 'phone-note'))
            self.card(im,pic,820,118,414,550)
            if not self.android.files: self.pending(im,'Phone layout preview · device recording pending')
        elif t<86:
            u=t-78
            self.text(im,'Automatically in sync.',640,38,59,center=True)
            if self.sync:
                left=self.sync_a.frame(u).crop((724,8,1432,890))
                right=self.sync_b.frame(u)
            else:
                left=crop('notes-prep' if u<1.3 else 'notes-desktop-edit' if u<6.1 else 'notes-phone-edit')
                right=crop('notes-prep' if u<2.6 else 'notes-desktop-edit' if u<4.8 else 'notes-phone-edit')
            self.card(im,left,50,184,730,435,True)
            self.card(im,right,900,151,325,497,True)
            self.text(im,'Mac',414,632,20,MUTED,False,True)
            self.text(im,'Phone layout',1063,660,20,MUTED,False,True)
            self.pending(im,'Live sync · two local instances · physical phone take pending' if self.sync
                         else 'Sync storyboard · simultaneous device recording pending')
        else:
            self.text(im,'superapp',640,178,120,center=True)
            self.text(im,'Open source.',640,340,52,MUTED,False,True)
            if self.url: self.text(im,self.url,640,493,28,WHITE,False,True)
        return im


def review_page(output,notes):
    links=''.join(f'<button data-t="{t}"><span>{t//60}:{t%60:02d}</span>{name}</button>' for t,name in CHAPTERS)
    page=f'''<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>superapp — first cut</title><style>
*{{box-sizing:border-box}}body{{margin:0;background:#101013;color:#f6f5f1;font:16px system-ui,sans-serif}}main{{max-width:1280px;margin:auto;padding:36px 24px}}h1{{font-size:40px;letter-spacing:-2px;margin:0}}.meta{{color:#a7a7b0;margin:8px 0 28px}}video{{display:block;width:100%;background:#000;border:1px solid #35353b}}nav{{display:flex;flex-wrap:wrap;gap:8px;margin:20px 0}}button{{background:#222226;color:inherit;border:1px solid #3a3a40;border-radius:6px;padding:10px 14px;cursor:pointer}}button:hover{{background:#393940}}button span{{color:#a6a6b1;margin-right:8px}}p{{max-width:900px;line-height:1.6}}a{{color:inherit}}.note{{color:#b9b9c2;font-size:14px}}</style>
<main><h1>superapp</h1><div class="meta">First cut · 90 seconds · original score</div>
<video controls preload="metadata" src="{output.name}"></video><nav>{links}</nav>
<p class="note">{notes}</p><p><a href="{output.name}" download>Download MP4</a> · <a href="score.wav" download>Score</a></p></main>
<script>const video=document.querySelector('video');document.querySelectorAll('button').forEach(b=>b.onclick=()=>{{video.currentTime=Number(b.dataset.t);video.play()}})</script></html>'''
    (OUT/'review.html').write_text(page)


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--width',type=int,default=1920); p.add_argument('--fps',type=int,default=60)
    p.add_argument('--url',default=''); p.add_argument('--stills',action='store_true')
    args=p.parse_args(); OUT.mkdir(parents=True,exist_ok=True)
    film=Film(args.width,args.fps,args.url)
    if args.stills:
        samples=[.2,2.7,11.3,14,21,33,41,47,55,63,72,82,88]
        thumbs=[]
        for t in samples:
            frame=film.render(t); frame.save(OUT/f'preview-{t:g}.jpg',quality=92)
            frame.thumbnail((480,270)); thumbs.append(frame)
        sheet=Image.new('RGB',(1440,270*math.ceil(len(thumbs)/3)),BG)
        for i,frame in enumerate(thumbs): sheet.paste(frame,(i%3*480,i//3*270))
        sheet.save(OUT/'film-contact-sheet.jpg',quality=92)
        print(OUT/'film-contact-sheet.jpg'); return
    if not (OUT/'score.wav').exists(): compose(OUT/'score.wav')
    output=OUT/'superapp-first-cut.mp4'
    temporary=OUT/'superapp-first-cut.rendering.mp4'
    cmd=['ffmpeg','-hide_banner','-loglevel','warning','-y','-f','rawvideo','-pix_fmt','rgb24',
         '-s',f'{film.width}x{film.height}','-r',str(args.fps),'-i','pipe:0','-i',str(OUT/'score.wav'),
         '-c:v','libx264','-preset','fast','-crf','18','-pix_fmt','yuv420p','-c:a','aac','-b:a','256k',
         '-af','loudnorm=I=-16:TP=-1.5:LRA=8','-movflags','+faststart','-t','90',str(temporary)]
    proc=subprocess.Popen(cmd,stdin=subprocess.PIPE)
    try:
        for frame in range(90*args.fps):
            if frame%(args.fps*5)==0: print(f'Rendering {frame//args.fps}/90 seconds',flush=True)
            proc.stdin.write(film.render(frame/args.fps).tobytes())
        proc.stdin.close()
        if proc.wait()!=0: raise RuntimeError('ffmpeg failed')
    except BaseException:
        proc.kill(); proc.wait(); raise
    temporary.replace(output)
    notes='Desktop app montage uses deterministic demo captures. '
    notes+='The speed passage uses real-time native Mac recording. ' if film.speed.files else 'Native speed recording is pending. '
    notes+='Android uses device footage. ' if film.android.files else 'Android currently uses the app’s phone layout preview. '
    notes+=('Both agent stories use verified live gateway results with prepared demo data. The meeting note was opened for its result shot; the language lesson is playable. Agent processing waits are cut. '
            if film.meeting and film.language else 'One or more agent stories still await verified live captures. ')
    notes+=('The sync ending records actual bidirectional edits between two separate local stores over loopback, one window using the phone layout. It does not demonstrate phone or network latency. '
            if film.sync else 'The sync ending is a labelled storyboard awaiting live capture. ')
    if not film.android.files: notes+='Physical Android and Mac-to-phone sync takes are pending because the connected phone is locked.'
    review_page(output,notes)
    (OUT/'edit-manifest.json').write_text(json.dumps({'duration':90,'fps':args.fps,'size':[film.width,film.height],'chapters':CHAPTERS,'notes':notes,'url':args.url},indent=2)+'\n')
    print(output,flush=True)


if __name__=='__main__': main()
