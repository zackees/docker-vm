"""Run inside the desktop image with /tmp and XDG_RUNTIME_DIR mounted as tmpfs."""
import os
import json
from pathlib import Path
import runpy
import signal
import subprocess
import time
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

watch = runpy.run_path('/usr/local/bin/crash-watch')


class CrashCaptureTests(unittest.TestCase):
    def test_shared_memory_low_water_mark_survives_recovery(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            diagnostics = watch['Diagnostics'](root / 'reports', root / 'missing-cgroup')
            for number in range(125):
                free = 0 if number == 2 else 100
                usage = SimpleNamespace(f_bavail=free, f_frsize=4096, f_blocks=100)
                with patch('os.statvfs', return_value=usage):
                    diagnostics.poll(force=True)
            snapshot = json.loads((root / 'reports/resources.json').read_text())
            self.assertEqual(snapshot['/dev/shm']['available_bytes'], 409600)
            self.assertEqual(snapshot['minimum_available']['/dev/shm']['available_bytes'], 0)
            self.assertEqual(len(snapshot['recent_samples']), 120)
            self.assertNotIn('recent_samples', snapshot['recent_samples'][0])
            self.assertNotIn('minimum_available', snapshot['recent_samples'][0])

    def test_live_resources_and_output_are_bounded_and_private(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            counters = root / 'cgroup'
            counters.mkdir()
            diagnostics = watch['Diagnostics'](root / 'reports', counters)
            for number in range(40):
                (counters / 'pids.events').write_text(f'max {number}\n')
                (counters / 'pids.current').write_text('512\n')
                diagnostics.poll(b'x' * (watch['MAX_REPORT_BYTES'] + 100), force=True)
            resource_file = root / 'reports/resources.json'
            snapshot = json.loads(resource_file.read_text())
            self.assertEqual(snapshot['pids.events'], 'max 39')
            self.assertEqual(snapshot['pids.current'], '512')
            self.assertEqual(len(snapshot['recent_counter_changes']), 32)
            self.assertEqual(resource_file.stat().st_mode & 0o777, 0o600)
            self.assertEqual((root / 'reports/browser-output.log').stat().st_size, watch['MAX_REPORT_BYTES'])

    def test_normal_browser_close_preserves_session(self):
        supervisor = subprocess.Popen(['/usr/local/bin/crash-watch', '/usr/bin/true'],
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(1)
            self.assertIsNone(supervisor.poll())
        finally:
            supervisor.terminate()
            supervisor.wait(timeout=5)

    def test_fatal_signal_produces_bounded_private_backtrace(self):
        for name in ['SIGTRAP', 'SIGABRT', 'SIGSEGV']:
            with self.subTest(signal=name):
                report = Path(os.environ['XDG_RUNTIME_DIR']) / f'crash/{name}.log'
                code = watch['run_debugger']([
                    '/usr/bin/python3', '-c',
                    f'import os,signal; print("x"*3000000, flush=True); os.kill(os.getpid(),signal.{name})',
                ], report)
                self.assertNotEqual(code, 0)
                text = report.read_text(errors='replace')
                self.assertIn(name, text)
                self.assertIn('#0', text)
                self.assertIn('Shared library addresses', text)
                self.assertLessEqual(report.stat().st_size, watch['MAX_REPORT_BYTES'])
                self.assertEqual(report.stat().st_mode & 0o777, 0o600)

    def test_normal_exit_is_not_a_crash(self):
        report = Path(os.environ['XDG_RUNTIME_DIR']) / 'crash/normal.log'
        self.assertEqual(watch['run_debugger'](['/usr/bin/true'], report), 0)

    def test_resume_signal_does_not_terminate_browser(self):
        report = Path(os.environ['XDG_RUNTIME_DIR']) / 'crash/resume.log'
        code = watch['run_debugger']([
            '/usr/bin/python3', '-c',
            'import os,signal; os.kill(os.getpid(),signal.SIGCONT); print("resumed normally")',
        ], report)
        self.assertEqual(code, 0)
        self.assertIn('resumed normally', report.read_text())

    def test_real_browser_crash_keeps_supervisor_and_report_alive(self):
        # Isolated synthetic headless session: never target an interactive user.
        report = Path(os.environ['XDG_RUNTIME_DIR']) / 'crash/browser-backtrace.log'
        report.unlink(missing_ok=True)  # Discard only the preceding test's report.
        supervisor = subprocess.Popen([
            '/usr/local/bin/crash-watch', '/usr/lib/chromium/chromium',
            '--headless', '--disable-gpu', '--no-first-run',
            '--user-data-dir=/tmp/crash-test-profile', 'about:blank',
        ], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            browser = None
            for _ in range(100):
                for path in Path('/proc').glob('[0-9]*/cmdline'):
                    try:
                        args = path.read_bytes().split(b'\0')
                        if args[0] == b'/usr/lib/chromium/chromium' and b'--user-data-dir=/tmp/crash-test-profile' in args:
                            browser = int(path.parent.name)
                    except OSError:
                        pass
                if browser is not None:
                    break
                time.sleep(0.1)
            self.assertIsNotNone(browser, 'Chromium did not start under GDB')
            time.sleep(2)
            os.kill(browser, signal.SIGTRAP)
            for _ in range(100):
                if report.exists():
                    break
                time.sleep(0.1)
            text = report.read_text(errors='replace')
            self.assertIn('SIGTRAP', text)
            self.assertIn('#0', text)
            self.assertIsNone(supervisor.poll(), 'Supervisor must preserve the tmpfs session')
        finally:
            supervisor.terminate()
            try:
                supervisor.wait(timeout=5)
            except subprocess.TimeoutExpired:
                supervisor.kill()
                supervisor.wait()


if __name__ == '__main__':
    unittest.main()
