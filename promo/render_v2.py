#!/usr/bin/env python3
"""The revised cut: uncropped app views and timestamped native GPU footage."""
import argparse
import bisect
import csv
from functools import lru_cache
import json
import math
from pathlib import Path
import subprocess
from PIL import Image, ImageDraw, ImageFont
from capture import OUT
from scenes import V2, HEROES

BG=(13,13,15); WHITE=(247,246,243); MUTED=(159,159,166)
CHAPTERS=[(0,'Apps'),(15,'A userspace OS'),(19,'One UI'),(31,'Fast'),
          (41,'SQLite'),(47,'Agents'),(59,'German practice'),(69,'Android'),
          (81,'Sync'),(88,'superapp')]


def ease(t): return 1-(1-max(0,min(1,t)))**3


@lru_cache(maxsize=32)
def font(size,bold=False):
    return ImageFont.truetype('/System/Library/Fonts/Supplemental/Arial Bold.ttf'
                             if bold else '/System/Library/Fonts/SFNS.ttf',size)


@lru_cache(maxsize=36)
def photo(path):
    with Image.open(path) as im: return im.convert('RGB')


class NativeFrames:
    """Nearest real frame, selected by its draw timestamp; no interpolation."""
    def __init__(self,name):
        self.path=V2/name/'frames'
        rows=list(csv.reader((self.path/'frames.csv').open()))
        self.times=[float(r[1])-float(rows[0][1]) for r in rows]
        extension='bmp' if (self.path/'000000.bmp').exists() else 'png'
        self.paths=[self.path/f'{int(r[0]):06}.{extension}' for r in rows]
        if len(self.paths)!=int((self.path/'complete').read_text()):
            raise RuntimeError('Incomplete native take '+name)

    def index(self,t):
        i=bisect.bisect_left(self.times,t)
        if i==0: return 0
        if i==len(self.times): return i-1
        return i if self.times[i]-t < t-self.times[i-1] else i-1

    def frame(self,t): return photo(str(self.paths[self.index(t)]))


class VideoFrames:
    """Full-resolution lossless decoding of the earlier verified sync take."""
    def __init__(self,name):
        path=OUT/'native'/name/'recording.mov'
        self.dest=V2/'decoded'/name;self.dest.mkdir(parents=True,exist_ok=True)
        if not (self.dest/'complete').exists():
            subprocess.run(['ffmpeg','-hide_banner','-loglevel','error','-y','-i',str(path),
                '-vf','fps=60','-frames:v','600',str(self.dest/'%05d.png')],check=True)
            (self.dest/'complete').write_text(str(path))
        self.files=sorted(self.dest.glob('*.png'))

    def frame(self,t): return photo(str(self.files[min(len(self.files)-1,max(0,round(t*60)))]))


class Film:
    def __init__(self,width=1920,previews=False):
        self.w=width;self.h=width*9//16;self.s=width/1920
        self.choreo=NativeFrames('choreography')
        self.speed=NativeFrames('speed') if (V2/'speed/frames/complete').exists() else None
        self.phone=NativeFrames('phone') if (V2/'phone/frames/complete').exists() else None
        self.android_desktop=NativeFrames('android-desktop') if (V2/'android-desktop/frames/complete').exists() else None
        self.sync_a=NativeFrames('sync-a') if (V2/'sync-a/frames/complete').exists() else None
        self.sync_b=NativeFrames('sync-b') if (V2/'sync-b/frames/complete').exists() else None
        self.old_a=None;self.old_b=None
        if not previews and self.sync_a is None:
            self.old_a=VideoFrames('sync-a');self.old_b=VideoFrames('sync-b')
        self.cues=json.loads((V2/'choreography/cues.json').read_text())
        self.names=list(HEROES)
        self.labels=['Mail','Files','Calendar','Workshop','Terminal','RSS','Notes',
                     'Agents','Telegram','Language\nlearning']

    def text(self,im,text,x,y,size=76,color=WHITE,bold=True,center=False):
        d=ImageDraw.Draw(im);f=font(round(size*self.s),bold)
        for line in text.split('\n'):
            width=d.textbbox((0,0),line,font=f)[2]
            px=x*self.s-width/2 if center else x*self.s
            d.text((round(px),round(y*self.s)),line,font=f,fill=color)
            y+=size*1.09

    def card(self,im,source,box,scale=1):
        # Always contain the complete recorded viewport. Never fit/crop/zoom it.
        x,y,w,h=box
        factor=min(w/source.width,h/source.height)*scale*self.s
        if factor>1.001: raise ValueError('A shot would enlarge its source pixels')
        sw=max(1,round(source.width*factor));sh=max(1,round(source.height*factor))
        px=round((x+w/2)*self.s-sw/2);py=round((y+h/2)*self.s-sh/2)
        pic=source.resize((sw,sh),Image.Resampling.LANCZOS)
        d=ImageDraw.Draw(im)
        d.rectangle((px-1,py-1,px+sw,py+sh),fill=(68,68,73))
        im.paste(pic,(px,py))
        return px,py,sw,sh

    def detail(self,name,shot='ready'):
        return photo(str(V2/'details'/name/f'{shot}.png'))

    def header(self,im,text): self.text(im,text,960,48,76,center=True)

    def hero(self,im,t):
        i=min(9,int(t/1.5));u=t-i*1.5;name=self.names[i]
        j=min(len(HEROES[name])-1,int(u/.75))
        src=self.detail('agent') if name=='agents' else photo(str(V2/'heroes'/f'{name}-{j}'/'frame.png'))
        label=self.labels[i]
        self.text(im,label,60+24*(1-ease(u/.18)),407 if i!=9 else 365,
                  82 if i not in (3,9) else 70)
        self.card(im,src,(530,144,1320,880),.985+.015*ease((u%(.75 if len(HEROES[name])>1 else 1.5))/.18))
        d=ImageDraw.Draw(im)
        for k in range(10):
            x=(64+k*35)*self.s
            d.rounded_rectangle((x,941*self.s,x+22*self.s,945*self.s),radius=2*self.s,
                                fill=WHITE if k==i else (57,57,62))

    def render(self,t):
        im=Image.new('RGB',(self.w,self.h),BG)
        screen=(64,224,1792,806.4)
        if t<15:
            self.hero(im,t)
        elif t<19:
            self.text(im,'superapp',64,48,87)
            # Same exact rectangle AND first frame as the following section.
            self.text(im,'A userspace OS.',1390,83,53,MUTED,False,True)
            self.card(im,self.choreo.frame(0),screen)
        elif t<31:
            u=t-19;self.header(im,'One UI. One way to work.')
            self.card(im,self.choreo.frame(u),screen)
            cue=next((c for c in reversed(self.cues) if c['time']<=u),None)
            if cue and u-cue['time']<1.2:
                verbs={'⌘,':'Stack','⌘.':'Split','⌘]':'Move','⌘T':'Tabs','⌘↑':'Switch tab',
                       '⌘2':'Workspace 2','⌘1':'Workspace 1'}
                self.text(im,verbs.get(cue['key'],'')+'  '+cue['key'],960,153,30,MUTED,False,True)
        elif t<41:
            u=t-31;self.text(im,'Fast.',64,33,114)
            self.text(im,'Makepad · Native GPU rendering',1285,100,37,MUTED,False,True)
            if self.speed is None: raise RuntimeError('The native speed take is missing')
            self.card(im,self.speed.frame(u+.1),screen)
            self.text(im,'⌘]' if u<4.95 else '⌘[',960,166,33,MUTED,False,True)
        elif t<47:
            u=t-41;self.header(im,'One shared database.')
            d=ImageDraw.Draw(im)
            for i,(name,label) in enumerate([('mail','Mail'),('telegram','Telegram'),('workshop','Workshop')]):
                x=90+i*604
                self.text(im,label,x+266,194,33,MUTED,False,True)
                self.card(im,self.detail('db-'+name),(x,250,532,545),.98+.02*ease((u-i*.12)/.3))
                x1=(x+266)*self.s;y1=808*self.s
                p=ease((u-.5-i*.08)/.55)
                d.line((x1,y1,x1+(960*self.s-x1)*p,y1+(895*self.s-y1)*p),
                       fill=(106,106,114),width=max(1,round(2*self.s)))
            d.rounded_rectangle(tuple(round(v*self.s) for v in (802,879,1118,995)),
                                radius=18*self.s,fill=WHITE)
            self.text(im,'SQLite',960,899,64,BG,True,True)
        elif t<59:
            u=t-47
            if u<1.5:
                self.text(im,'Agents.',87,230,160)
                self.text(im,'With context.',87,412,124,MUTED)
            elif u<4.5:
                self.header(im,'“Prep me for this meeting.”')
                self.card(im,self.detail('meeting'),(70,224,820,729))
                self.text(im,'Calendar',1260,343,51)
                self.text(im,'Mail',1260,439,51)
                self.text(im,'Telegram',1260,535,51)
                self.text(im,'⌘⇧A',1315,725,51,MUTED,False,True)
                d=ImageDraw.Draw(im)
                for j in range(3):
                    y=(380+j*96)*self.s
                    d.line((935*self.s,565*self.s,1170*self.s,y),fill=(101,101,108),width=2)
            elif u<7.5:
                self.header(im,'Across your apps.')
                self.card(im,self.detail('meeting-agent'),(280,205,1360,850))
            else:
                self.header(im,'Ready for the meeting.')
                self.card(im,self.detail('launch-note'),(300,200,1320,850))
        elif t<69:
            u=t-59
            self.header(im,'“Turn this into German practice.”')
            self.card(im,self.detail('bike'),(55,210,729,825))
            self.text(im,'→',914,555,73,MUTED,False,True)
            if u>1.1:
                self.card(im,self.detail('lesson','ready' if u<6.5 else 'answer'),
                          (1050,210,825,825),.975+.025*ease((u-1.1)/.35))
            else:
                self.text(im,'Berlin by bike',1460,442,48,center=True)
                self.text(im,'English → German',1460,520,32,MUTED,False,True)
        elif t<81:
            u=t-69;self.text(im,'Android, too.',66,48,89)
            # Keep the desktop comparison completely filled and steady so the
            # phone's gestures are the focus of this section.
            if self.android_desktop is None: raise RuntimeError('Fresh desktop comparison take is missing')
            self.card(im,self.android_desktop.frame(0),(55,353,1180,531))
            if self.phone is None: raise RuntimeError('Moving phone-layout take is missing')
            self.card(im,self.phone.frame(u+1.45),(1370,160,480,850))
            cues=[(0,'Overview'),(2,'Move panels'),(3.9,'Close panels'),(5.3,'Open a panel'),(6.5,'Workspaces'),(9,'Overview')]
            label=next(label for at,label in reversed(cues) if u>=at)
            self.text(im,label,645,922,34,MUTED,False,True)
            self.text(im,'Phone layout preview',1609,1025,23,MUTED,False,True)
        elif t<88:
            u=t-81;self.header(im,'Automatically in sync.')
            if self.sync_a:
                self.card(im,self.sync_a.frame(u+.15),(55,260,1200,660))
                self.card(im,self.sync_b.frame(u+.15),(1380,165,465,824))
            elif self.old_a:
                a=self.old_a.frame(u+.6);w,h=a.size
                # Pixel-exact boundary of the complete focused note panel.
                a=a.crop((round(w*724/1440),round(h*8/900),round(w*1432/1440),round(h*892/900)))
                self.card(im,a,(140,212,890,820))
                self.card(im,self.old_b.frame(u+.6),(1280,195,475,840))
            else:
                self.card(im,self.detail('launch-note'),(80,260,1200,680))
            self.text(im,'Mac',650,970,30,MUTED,False,True)
            self.text(im,'Two local instances · phone layout preview',960,1034,23,MUTED,False,True)
        else:
            self.text(im,'superapp',960,380,184,center=True)
        return im


def review_page(path):
    buttons=''.join(f'<button data-t="{t}">{t//60}:{t%60:02} {label}</button>' for t,label in CHAPTERS)
    text=f'''<!doctype html><meta charset="utf-8"><title>superapp · revised cut</title>
    <style>body{{margin:36px auto;padding:0 24px;max-width:1300px;background:#0d0d0f;color:#f7f6f3;font:17px system-ui}}
    video{{width:100%;background:#000}}button{{padding:10px 14px;margin:7px 6px 0 0;background:#252528;color:white;border:0;border-radius:6px;cursor:pointer}}a{{color:inherit}}p{{color:#b5b5bb;line-height:1.5}}</style>
    <h1>superapp · revised cut</h1><video controls playsinline src="{path.name}"></video><nav>{buttons}</nav>
    <p>1920 × 1080 · 60 fps · 90 seconds · original music. New native Retina captures;
    every app is labelled and framed in full. Motion comes from real-time GPU frames.
    Both agent results use actual model and tool calls against prepared demo records.
    Phone footage uses the app’s phone layout on Mac. The physical phone disconnected;
    sync footage demonstrates two local stores, not Mac-to-Android latency.</p>
    <p><a href="{path.name}" download>Download MP4</a> · <a href="v2/review-report.json">Review data</a></p>
    <script>let v=document.querySelector('video');document.querySelectorAll('button').forEach(b=>b.onclick=()=>{{v.currentTime=+b.dataset.t;v.play()}})</script>'''
    (OUT/'review-v2.html').write_text(text)


def main():
    p=argparse.ArgumentParser();p.add_argument('--stills',action='store_true');p.add_argument('--width',type=int,default=1920)
    a=p.parse_args();film=Film(a.width,a.stills)
    if a.stills:
        times=[.3,1.9,3.3,7.8,11,14.2,17,20,26.8,32,43.5,49.5,53,57,60,63,67,72,77,83,89]
        contact=Image.new('RGB',(1440,270*math.ceil(len(times)/3)),BG)
        for i,t in enumerate(times):
            im=film.render(t);im.save(V2/f'preview-{t:g}.png');im.thumbnail((480,270))
            contact.paste(im,(i%3*480,i//3*270))
        contact.save(V2/'film-contact.png');print(V2/'film-contact.png');return
    temporary=OUT/'superapp-v2.rendering.mp4';path=OUT/'superapp-v2.mp4'
    cmd=['ffmpeg','-hide_banner','-loglevel','warning','-y','-f','rawvideo','-pix_fmt','rgb24',
         '-s',f'{film.w}x{film.h}','-r','60','-i','pipe:0','-i',str(V2/'score.wav'),
         '-c:v','libx264','-preset','fast','-crf','16','-pix_fmt','yuv420p',
         '-c:a','aac','-b:a','256k','-af','loudnorm=I=-16:TP=-2.5:LRA=8',
         '-movflags','+faststart','-t','90',str(temporary)]
    proc=subprocess.Popen(cmd,stdin=subprocess.PIPE)
    try:
        for n in range(5400):
            if n%300==0:print(f'Rendering {n//60}/90 s',flush=True)
            proc.stdin.write(film.render(n/60).tobytes())
        proc.stdin.close()
        if proc.wait()!=0:raise RuntimeError('Video encoding failed')
    except BaseException:
        proc.kill();proc.wait();raise
    temporary.replace(path);review_page(path)
    (V2/'edit-manifest.json').write_text(json.dumps({'duration':90,'fps':60,'size':[film.w,film.h],
        'chapters':CHAPTERS,'phone':'native Mac phone-layout preview','motion':'native GPU frames with real draw timestamps'},indent=2)+'\n')
    print(path,flush=True)


if __name__=='__main__':main()
