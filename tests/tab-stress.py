"""Synthetic multi-site tab load; run only in an isolated test container.

The private test-only DevTools endpoint is loopback-only and is not enabled in
the desktop application. No remote user content is inspected by this test.
"""
import json
import os
from pathlib import Path
import subprocess
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Page(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b'<title>Synthetic tab load</title><script>window.load=new Uint8Array(8*1024*1024);load.fill(42)</script>Test tab'
        if os.environ.get('STRESS_GRAPHICS') == '1':
            body += b'''<canvas width="4096" height="4096"></canvas><script>
const ctx=document.querySelector('canvas').getContext('2d');
ctx.fillStyle='blue';ctx.fillRect(0,0,4096,4096);
setInterval(()=>{ctx.fillStyle=`rgb(${Math.random()*255},50,80)`;
ctx.fillRect(0,0,4096,4096)},250);
</script>'''
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


server = ThreadingHTTPServer(('127.0.0.1', 8080), Page)
threading.Thread(target=server.serve_forever, daemon=True).start()
browser = subprocess.Popen([
    '/usr/local/bin/crash-watch', '/usr/lib/chromium/chromium',
    '--headless', '--disable-gpu', '--no-first-run', '--site-per-process',
    '--user-data-dir=/tmp/tab-stress-profile',
    '--remote-debugging-address=127.0.0.1', '--remote-debugging-port=9222',
    '--host-resolver-rules=MAP *.test 127.0.0.1', 'about:blank',
], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def request(path, method='GET'):
    with urllib.request.urlopen(urllib.request.Request('http://127.0.0.1:9222' + path, method=method), timeout=5) as reply:
        return json.load(reply)


try:
    for _ in range(100):
        try:
            request('/json/version')
            break
        except OSError:
            time.sleep(.1)
    tabs = int(os.environ.get('STRESS_TABS', '48'))
    for number in range(tabs):
        tab = request(f'/json/new?http://tab-{number}.test:8080/', 'PUT')
        if os.environ.get('STRESS_GRAPHICS') == '1':
            with urllib.request.urlopen('http://127.0.0.1:9222/json/activate/' + tab['id'], timeout=5):
                pass
        time.sleep(.5 if os.environ.get('STRESS_GRAPHICS') == '1' else .1)
    time.sleep(5)
    pages = [page for page in request('/json/list') if page.get('type') == 'page']
    events = Path('/sys/fs/cgroup/pids.events').read_text().strip()
    loaded = sum(page.get('title') == 'Synthetic tab load' for page in pages)
    dumps = len(list(Path('/home/desktop/.config/chromium/Crash Reports').rglob('*.dmp')))
    resources = json.loads(Path('/run/user/1000/crash/resources.json').read_text())
    print(json.dumps({'requested_tabs': tabs, 'live_pages': len(pages), 'loaded_pages': loaded,
                      'pids_events': events, 'crash_dumps': dumps,
                      'shm': resources['/dev/shm'],
                      'minimum_shm': resources['minimum_available']['/dev/shm'],
                      'pids_current': Path('/sys/fs/cgroup/pids.current').read_text().strip(),
                      'memory_events': Path('/sys/fs/cgroup/memory.events').read_text().strip()}), flush=True)
    assert len(pages) >= tabs
    assert events == 'max 0', 'task limit exhausted'
    assert loaded == tabs, 'some tabs failed to load or crashed'
    assert dumps == 0, 'Chromium child process crashed'
    assert browser.poll() is None
finally:
    browser.terminate()
    try:
        browser.wait(timeout=5)
    except subprocess.TimeoutExpired:
        browser.kill()
        browser.wait()
    server.shutdown()
