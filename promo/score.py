#!/usr/bin/env python3
"""Original 120 BPM electronic score and tactile effects, synthesized locally."""
from pathlib import Path
import wave
import numpy as np

RATE = 48000
DURATION = 90


def compose(path, revision=1):
    rng = np.random.default_rng(20260914)
    music = np.zeros((RATE * DURATION, 2), np.float32)
    effects = np.zeros_like(music)

    def add(signal, at, gain=1.0, pan=0.0, target=music):
        start = round(at * RATE)
        if start < 0 or start >= len(target):
            return
        n = min(len(signal), len(target) - start)
        stereo = np.array([np.sqrt((1-pan)/2), np.sqrt((1+pan)/2)], np.float32)
        target[start:start+n] += signal[:n, None] * (gain * stereo)

    def clock(seconds):
        return np.arange(round(seconds * RATE), dtype=np.float32) / RATE

    def note(midi, seconds, kind='pluck'):
        t = clock(seconds)
        f = 440 * 2 ** ((midi-69)/12)
        if kind == 'bass':
            sig = np.sin(2*np.pi*f*t) + .25*np.sin(4*np.pi*f*t) + .08*np.sin(6*np.pi*f*t)
            env = np.minimum(t/.008, 1)*np.exp(-t*4)*np.minimum((seconds-t)/.05, 1)
        elif kind == 'pad':
            sig = (np.sin(2*np.pi*f*t) + .5*np.sin(2*np.pi*f*1.003*t) + .2*np.sin(4*np.pi*f*t))/1.7
            env = np.minimum(t/.23, 1)*np.minimum((seconds-t)/.7, 1)
        else:
            sig = np.sin(2*np.pi*f*t) + .35*np.sin(4*np.pi*f*t)*np.exp(-t*10) + .12*np.sin(6*np.pi*f*t)
            env = np.minimum(t/.003, 1)*np.exp(-t*7)
        return (sig * np.maximum(env, 0)).astype(np.float32)

    t = clock(.45)
    phase = 2*np.pi*(48*t + 100*.018*(1-np.exp(-t/.018)))
    kick = np.sin(phase)*np.exp(-t*12) + .08*rng.normal(size=len(t))*np.exp(-t*170)
    t = clock(.22)
    noise = rng.normal(size=len(t)).astype(np.float32)
    clap = np.diff(noise, prepend=0)*np.exp(-t*22)*.35 + np.sin(2*np.pi*185*t)*np.exp(-t*30)*.25
    t = clock(.07)
    hat = np.diff(rng.normal(size=len(t)), prepend=0)*np.exp(-t*65)*.12
    roots = [36, 32, 39, 34]
    chords = [[60,63,67,70], [56,60,63,67], [58,63,67,70], [58,62,65,69]]
    melody = [72, 79, 75, 82, 79, 75, 74, 79]
    for bar in range(44 if revision==2 else 43):
        at = bar*2
        quiet = (41 <= at < 49 or 81 <= at < 88) if revision==2 else (38 <= at < 48 or 78 <= at < 86)
        level = .5 if quiet else 1
        if at < (81 if revision==2 else 78):
            for beat in (0, 1, 2, 3):
                if not quiet or beat in (0,2):
                    add(kick, at+beat*.5, .50*level)
            for beat in (1,3):
                if not quiet:
                    add(clap, at+beat*.5, .24, .08)
            fast=(31 <= at < 41) if revision==2 else (28 <= at < 38)
            for step in range(16 if fast else 8):
                count = 16 if fast else 8
                add(hat, at+step*2/count, (.18 if step%2 else .30)*level, (-1 if step%2 else 1)*.25)
        if at >= 4 and at < (81 if revision==2 else 78):
            for offset, degree in [(0,0),(.75,0),(1.25,12),(1.75,7)]:
                add(note(roots[(bar//2)%4]+degree, .4, 'bass'), at+offset, .23*level)
        if bar%2 == 0 and at >= 12:
            for n in chords[(bar//2)%4]:
                add(note(n, 3.8, 'pad'), at, .055 if quiet else .073, (n%3-1)*.5)
        melodic=(at < 15 or 19 <= at < 41 or 59 <= at < 81) if revision==2 else (at < 12 or 16 <= at < 38 or 58 <= at < 78)
        if melodic:
            for step in (0, 2, 3, 6):
                n = melody[(bar+step)%len(melody)]
                pos = at+step*.25
                tone = note(n, .8)
                add(tone, pos, .075 if at < 12 else .10, -.35)
                add(tone, pos+.375, .022, .5)
    # Sound follows the key editorial actions, including the two-device answer.
    clicks=([i*1.5+j*.75 for i in range(10) for j in range(1 if i in (4,7) else 2)]
            +[19.15,20.65,22.15,23.65,25,26.3,28,65.5]) if revision==2 else [0,.5,1,1.5,2,2.5,3,3.5,4,4.5,5,5.5,6,6.5,7,7.5,8,9,10,11,17.5,19,20.5,22,24,26]
    for at in clicks:
        t=clock(.035)
        click=(rng.normal(size=len(t))*.1+np.sin(2*np.pi*1700*t)*.14)*np.exp(-t*170)
        add(click, at, .18, target=effects)
    for at in ([15,19,31,41,47,59,69,81,88] if revision==2 else [12,28,38,44,58,66]):
        t=clock(.28)
        slide=rng.normal(size=len(t))*np.sin(np.pi*t/.28)**2*.035
        add(slide, at-.16, .6, target=effects)
        add(note(48,.65,'bass'), at, .24, target=effects)
    sync_notes=[(81.75,79,-.55),(82.1,84,.55),(83.4,79,.55),(83.75,84,-.55)] if revision==2 else [(79.3,79,-.55),(80.6,84,.55),(82.8,79,.55),(84.1,84,-.55)]
    for at, n, pan in sync_notes:
        add(note(n, .8), at, .12, pan, effects)
    end=88 if revision==2 else 86
    for n in [48,60,63,67,74]:
        add(note(n,1.95 if revision==2 else 3.8,'pad'),end,.11)
    add(note(79,1.5),end,.16,-.15,effects)
    add(note(84,1.45),end+.5,.13,.15,effects)
    fade = np.minimum(np.arange(len(music))/240, 1)*np.minimum((len(music)-np.arange(len(music)))/48000, 1)
    combined = np.tanh((music+effects)*1.2)*fade[:,None]
    combined *= .87/max(float(np.abs(combined).max()), .01)
    path = Path(path); path.parent.mkdir(parents=True, exist_ok=True)
    for filename, samples in [(path,combined), (path.with_name('music-stem.wav'),music), (path.with_name('effects-stem.wav'),effects)]:
        pcm=(np.clip(samples,-1,1)*32767).astype('<i2')
        with wave.open(str(filename),'wb') as out:
            out.setparams((2,2,RATE,0,'NONE','not compressed'))
            out.writeframes(pcm.tobytes())
    return path


if __name__ == '__main__':
    print(compose(Path(__file__).resolve().parents[1]/'.context/promo/score.wav'))
