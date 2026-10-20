"""Output changes must move Xfce's hidden dock, including RFB-driven resizes."""
import runpy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import subprocess

controller = runpy.run_path(str(Path(__file__).resolve().parents[1] / 'images/desktop/wayland-session'))
Output = controller['Output']
refresh = controller['refresh_hidden_panels']


class OutputTests(unittest.TestCase):
    def test_external_resize_and_scale_refresh_once_per_change(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Output({}, '1280x800', Path(directory) / 'generation')
            with patch.object(Output, 'current', side_effect=[
                '1280x800@1', '1280x800@1', '1920x1080@1', '1920x1080@1.5', '1920x1080@1.5'
            ]), patch.dict(Output.watch.__globals__, refresh_hidden_panels=unittest.mock.Mock()) as scope:
                for _ in range(5):
                    output.watch()
                self.assertEqual(scope['refresh_hidden_panels'].call_count, 2)
                self.assertEqual(output.generation_file.read_text(), '2')

    def test_only_hidden_panels_are_refreshed_and_behavior_is_preserved(self):
        listed = subprocess.CompletedProcess([], 0, '/panels/panel-1/autohide-behavior 0\n'
                                             '/panels/panel-2/autohide-behavior 1\n'
                                             '/panels/panel-7/autohide-behavior 2\n')
        with patch('subprocess.run', side_effect=[listed, *[subprocess.CompletedProcess([], 0)] * 4]) as run:
            refresh({'WAYLAND_DISPLAY': 'test'})
            self.assertEqual([call.args[0][-3:] for call in run.call_args_list[1:]], [
                ['/panels/panel-2/autohide-behavior', '-s', '0'],
                ['/panels/panel-2/autohide-behavior', '-s', '1'],
                ['/panels/panel-7/autohide-behavior', '-s', '0'],
                ['/panels/panel-7/autohide-behavior', '-s', '2']])

    def test_restore_is_attempted_even_if_disabling_times_out(self):
        listed = subprocess.CompletedProcess([], 0, '/panels/panel-2/autohide-behavior 2\n')
        with patch('subprocess.run', side_effect=[listed, subprocess.TimeoutExpired('xfconf-query', 2),
                                               subprocess.CompletedProcess([], 0)]) as run:
            refresh({})
            self.assertEqual(run.call_args.args[0][-1], '2')


if __name__ == '__main__':
    unittest.main()
