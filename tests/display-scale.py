"""Container DPI settings tests; no GTK/display or persistent session required."""
from pathlib import Path
import types
import unittest
from unittest.mock import patch

controller = types.ModuleType('display_scale')
source = Path(__file__).resolve().parents[1] / 'images/desktop/display-scale'
exec(compile(source.read_text(), str(source), 'exec'), controller.__dict__)


class DisplayScaleTests(unittest.TestCase):
    def test_host_scale_becomes_container_dpi_and_panel_sizes(self):
        for scale in [1, 1.25, 1.5, 1.75, 2]:
            with self.subTest(scale=scale), patch.object(controller.subprocess, 'run') as run:
                controller.apply_scale(scale)
                commands = [call.args[0] for call in run.call_args_list]
                self.assertIn(['xfconf-query', '-c', 'xsettings', '-p', '/Xft/DPI',
                               '-n', '-t', 'int', '-s', str(round(96 * scale))], commands)
                self.assertIn(['xrandr', '--dpi', str(round(96 * scale))], commands)
                xrdb = next(call for call in run.call_args_list if call.args[0][0] == 'xrdb')
                self.assertEqual(xrdb.kwargs['input'], f'Xft.dpi: {round(96 * scale)}\n')

    def test_invalid_or_unbounded_host_values_never_run_commands(self):
        for value in ['nan', 'inf', '-1', '0', '4.1', '$(touch bad)', 'hello']:
            with self.subTest(value=value), patch.object(controller.subprocess, 'run') as run:
                with self.assertRaises(ValueError):
                    controller.apply_scale(value)
                run.assert_not_called()

    def test_valid_endpoints(self):
        self.assertEqual(controller.valid_scale('0.5'), 0.5)
        self.assertEqual(controller.valid_scale('4'), 4)


if __name__ == '__main__':
    unittest.main()
