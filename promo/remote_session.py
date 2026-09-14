"""An owned background window, controlled without global keyboard events."""
import json
import os
import re
import subprocess
import time
import urllib.parse
import urllib.request
from capture import OUT, ROOT


class RemoteSession:
    def __init__(self, db, dest, window='1440x900', extra_args=(), env=None, binary=None):
        self.db = db
        self.dest = dest
        self.window_size = window
        self.extra_args = list(extra_args)
        self.env = env
        self.binary = binary or ROOT/'target/debug/superapp'
        self.proc = None
        self.recording = None
        self.events = []

    def frontmost(self):
        return int(subprocess.check_output(
            [str(OUT/'bin/window-id'), '--frontmost'], text=True).strip())

    def check_focus(self):
        current = self.frontmost()
        if current == self.proc.pid:
            raise RuntimeError('Capture app unexpectedly became the foreground app')
        return current

    def __enter__(self):
        self.dest.mkdir(parents=True, exist_ok=True)
        self.before = self.frontmost()
        self.log = (self.dest/'app.log').open('w')
        self.started = time.monotonic()
        env = dict(os.environ if self.env is None else self.env)
        env.pop('MAKEPAD_FOCUS', None)
        env['MAKEPAD_NO_FOCUS'] = '1'
        env['MAKEPAD_PRESENT_WHEN_OCCLUDED'] = '1'
        self.proc = subprocess.Popen(
            [str(self.binary), '--db', str(self.db),
             '--window', self.window_size, '--remote'] + self.extra_args, cwd=ROOT,
            stdout=self.log, stderr=subprocess.STDOUT, env=env)
        try:
            for _ in range(150):
                text = (self.dest/'app.log').read_text()
                match = re.search(r'listening on 127\.0\.0\.1:(\d+)', text)
                if match:
                    self.port = match.group(1)
                    break
                if self.proc.poll() is not None:
                    raise RuntimeError('Capture app exited during startup')
                time.sleep(.1)
            else:
                raise TimeoutError('No remote control port')
            time.sleep(1.5)
            self.after = self.check_focus()
            dimensions = self.window_size.split('x')
            self.window = None
            for _ in range(10):
                window = subprocess.run(
                    [str(OUT/'bin/window-id'), str(self.proc.pid)] + dimensions,
                    capture_output=True, text=True)
                if window.returncode == 0:
                    self.window = window.stdout.strip()
                    break
                if self.proc.poll() is not None:
                    raise RuntimeError('Capture app exited before its window appeared')
                time.sleep(.1)
            # Direct GPU captures do not depend on WindowServer enumeration.
            # A covered/off-space window can still render its own Metal output.
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def call(self, path, **params):
        url = f'http://127.0.0.1:{self.port}/{path}?'+urllib.parse.urlencode(params)
        with urllib.request.urlopen(url, timeout=20) as response:
            data = response.read()
        self.events.append({'time': time.monotonic()-self.started, 'event': path,
                            'params': params, 'frontmost_pid': self.check_focus()})
        return data

    def shot(self, name):
        (self.dest/f'{name}.png').write_bytes(self.call('g', raw=1))

    def record(self, seconds, name='recording.mov'):
        if self.window is None: raise RuntimeError('No WindowServer window for screen recording')
        path = self.dest/name
        if path.exists():
            path.replace(path.with_name(f'{path.stem}-{time.time_ns()}{path.suffix}'))
        self.recording = subprocess.Popen(
            ['/usr/sbin/screencapture', '-x', '-o', '-v', f'-V{seconds}',
             f'-l{self.window}', str(path)])

    def record_precise(self, seconds, name='recording.mov'):
        if self.window is None: raise RuntimeError('No WindowServer window for screen recording')
        path = self.dest/name
        if path.exists():
            raise RuntimeError(f'Archive the previous take first: {path}')
        ready = path.with_suffix(path.suffix+'.ready')
        self.recording = subprocess.Popen(
            [str(OUT/'bin/record-window'), self.window, str(path), str(seconds)],
            stdout=self.log, stderr=subprocess.STDOUT)
        deadline = time.monotonic()+10
        while not ready.exists():
            if self.recording.poll() is not None:
                raise RuntimeError('Window recording exited before its first frame')
            if time.monotonic()>deadline:
                raise TimeoutError('Window capture did not produce a frame')
            time.sleep(.01)
        self.record_start = time.monotonic()
        return self.record_start

    def record_gpu(self):
        frames = self.dest/'frames'
        if not frames.is_dir():
            raise RuntimeError('Native GPU recorder did not initialize')
        (frames/'start').write_text('start bounded real-time capture\n')
        deadline = time.monotonic()+10
        while not (frames/'ready').exists():
            if self.proc.poll() is not None or time.monotonic()>deadline:
                raise RuntimeError('Native GPU recorder produced no frame')
            time.sleep(.002)
        self.record_start = time.monotonic()
        return self.record_start

    def finish_gpu(self):
        frames = self.dest/'frames'
        deadline = time.monotonic()+125
        while True:
            done = frames/'complete'
            count = len(list(frames.glob('*.png')))+len(list(frames.glob('*.bmp')))
            if done.exists() and count == int(done.read_text()):
                break
            if self.proc.poll() is not None or time.monotonic()>deadline:
                raise RuntimeError('Native GPU recording did not finish')
            time.sleep(.05)
        self.check_focus()

    def __exit__(self, *_):
        if self.recording and self.recording.poll() is None:
            self.recording.wait()
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        self.log.close()
        (self.dest/'session.json').write_text(json.dumps({
            'source_database': str(self.db), 'pid': self.proc.pid,
            'frontmost_before': self.before, 'frontmost_after': getattr(self, 'after', None),
            'events': self.events}, indent=2)+'\n')
