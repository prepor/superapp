#!/usr/bin/env python3
"""Third cut. Reuse accepted v2 footage and replace only the revised sections."""
import argparse
import json
import math
import subprocess
from PIL import Image, ImageDraw
from capture import OUT
from scenes import V2
from scenes_v3 import V3
from render_v2 import Film, NativeFrames, photo, ease, BG, WHITE, MUTED, CHAPTERS


class FilmV3(Film):
    def __init__(self,width=1920,previews=False):
        super().__init__(width,previews)
        self.ui=[NativeFrames(str(V3/name)) for name in ['ui-launch','ui-mail','ui-files']]
        self.offsets=[]
        for name in ['ui-launch','ui-mail','ui-files']:
            cues=json.loads((V3/name/'cues.json').read_text())
            self.offsets.append(max(0,cues[0]['time']-.15))
        self.phone=NativeFrames(str(V3/'phone'))
        self.phone_offset=json.loads((V3/'phone/cues.json').read_text())[0]['time']-.12

    def hero(self,im,t):
        i=min(9,int(t/1.5));u=t-i*1.5;name=self.names[i]
        if name not in ('calendar','telegram'):
            return super().hero(im,t)
        j=int(u/.75)
        src=photo(str(V3/'heroes'/f'{name}-{j}'/'frame.png'))
        self.text(im,self.labels[i],60+24*(1-ease(u/.18)),407,82)
        self.card(im,src,(530,144,1320,880),.985+.015*ease((u%.75)/.18))
        d=ImageDraw.Draw(im)
        for k in range(10):
            x=(64+k*35)*self.s
            d.rounded_rectangle((x,941*self.s,x+22*self.s,945*self.s),radius=2*self.s,
                                fill=WHITE if k==i else (57,57,62))

    def render(self,t):
        if not (15<=t<31 or 69<=t<81):
            return super().render(t)
        im=Image.new('RGB',(self.w,self.h),BG)
        screen=(64,224,1792,806.4)
        if t<19:
            self.text(im,'superapp',64,48,87)
            self.text(im,'A userspace OS.',1390,83,53,MUTED,False,True)
            self.card(im,self.ui[0].frame(self.offsets[0]),screen)
        elif t<31:
            u=t-19;self.header(im,'One UI. One way to work.')
            i=0 if u<3.5 else 1 if u<7.5 else 2
            local=u-[0,3.5,7.5][i]
            self.card(im,self.ui[i].frame(self.offsets[i]+local),screen)
            if i==0:
                cue='Find any panel.  ⌘⌘' if local<1.85 else 'Stack ⌘,   Split ⌘.'
            elif i==1:
                cue='Filter. Preview. Select.'
            else:
                cue='The same controls in Files.' if local<2.3 else 'Stack. Tabs. Switch.'
            self.text(im,cue,960,153,30,MUTED,False,True)
        else:
            u=t-69;self.text(im,'Android, too.',66,48,89)
            self.card(im,photo(str(V3/'android-desktop/frame.png')),(55,267,1125,750))
            self.card(im,self.phone.frame(self.phone_offset+u),(1300,78,540,956.25))
            cues=[(0,'Open Overview'),(.65,'Switch workspaces'),(1.62,'Drag. Stack. Drop.'),
                  (3.34,'Swipe to close'),(4.35,'Open a panel'),(5.5,'Back to Overview'),
                  (6.62,'Make a new column'),(8.25,'Open a panel'),(9.05,'Explore Overview'),
                  (11.1,'Back to work')]
            cue=next(label for at,label in reversed(cues) if u>=at)
            self.text(im,cue,610,190,37,MUTED,False,True)
            self.text(im,'Phone layout preview',1570,1045,23,MUTED,False,True)
        return im


def review_page(path):
    buttons=''.join(f'<button data-t="{t}">{t//60}:{t%60:02} {label}</button>' for t,label in CHAPTERS)
    (OUT/'review-v3.html').write_text(f'''<!doctype html><meta charset="utf-8"><title>superapp · cut 3</title>
    <style>body{{margin:36px auto;padding:0 24px;max-width:1300px;background:#0d0d0f;color:#f7f6f3;font:17px system-ui}}
    video{{width:100%;background:#000}}button{{padding:10px 14px;margin:7px 6px 0 0;background:#252528;color:white;border:0;border-radius:6px;cursor:pointer}}a{{color:inherit}}p{{color:#b5b5bb;line-height:1.5}}</style>
    <h1>superapp · cut 3</h1><video controls playsinline src="{path.name}"></video><nav>{buttons}</nav>
    <p>1920 × 1080 · 60 fps · 90 seconds · original music. Populated calendar, Telegram photo and viewer,
    launcher search, shared filters, previews and selection. Overview input moves across actual native frames:
    open, switch workspace, drag and drop, open a panel and swipe to close.</p>
    <p>The phone sequence is the native phone layout on Mac; the physical Android device is disconnected.
    Sync uses two local stores. The sample Telegram photograph is generated; UI and input are real.</p>
    <p><a href="{path.name}" download>Download MP4</a> · <a href="v3/review-report.json">Review data</a>
    · <a href="v3/assets/provenance.json">Sample photo and prompt</a></p>
    <script>let v=document.querySelector('video');document.querySelectorAll('button').forEach(b=>b.onclick=()=>{{v.currentTime=+b.dataset.t;v.play()}})</script>''')


def main():
    p=argparse.ArgumentParser();p.add_argument('--stills',action='store_true');a=p.parse_args()
    film=FilmV3(previews=a.stills)
    if a.stills:
        times=[3.3,4.1,12.3,13.1,17,19.6,20.3,21.2,22,23,24,25.5,26.8,28,29.4,30.3,
               69.4,70,71.3,72,73,74.8,75.3,76.7,77.5,78.5,80.3]
        contact=Image.new('RGB',(1440,270*math.ceil(len(times)/3)),BG)
        for i,t in enumerate(times):
            im=film.render(t);im.save(V3/f'preview-{t:g}.png');im.thumbnail((480,270))
            contact.paste(im,(i%3*480,i//3*270))
        contact.save(V3/'film-contact.png');print(V3/'film-contact.png');return
    temp=OUT/'superapp-v3.rendering.mp4';path=OUT/'superapp-v3.mp4'
    cmd=['ffmpeg','-hide_banner','-loglevel','warning','-y','-f','rawvideo','-pix_fmt','rgb24',
         '-s','1920x1080','-r','60','-i','pipe:0','-i',str(V2/'score.wav'),
         '-c:v','libx264','-preset','fast','-crf','16','-pix_fmt','yuv420p',
         '-c:a','aac','-b:a','256k','-af','loudnorm=I=-16:TP=-2.5:LRA=8',
         '-movflags','+faststart','-t','90',str(temp)]
    process=subprocess.Popen(cmd,stdin=subprocess.PIPE)
    try:
        for i in range(5400):
            process.stdin.write(film.render(i/60).tobytes())
            if i%600==0:print(f'Rendered {i//60}/90 seconds',flush=True)
        process.stdin.close()
        if process.wait()!=0:raise RuntimeError('Encoding failed')
    except BaseException:
        process.kill();process.wait();raise
    temp.replace(path);review_page(path)
    (V3/'edit-manifest.json').write_text(json.dumps({'duration':90,'fps':60,'size':[1920,1080],
        'ui_offsets':film.offsets,'phone_offset':film.phone_offset,'chapters':CHAPTERS,
        'native_capture':True,'interpolation':False,'android_device':False},indent=2)+'\n')
    print(path,flush=True)


if __name__=='__main__':main()
