"""AI Image Enlarger: protocol v2 UI (inspector panel, asset action and tool window).

Runs in the plugin's UI process (standard library only). The enlargement itself runs in a child
process through this folder's `run` launcher (see pigeon_ui.run_child).
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import ui_support as ui  # noqa: E402


class Enlarge(ui.ImageTool):
    OPERATION = 'enlarge'
    ACTION_ID = 'open'
    TITLE = 'AI Image Enlarger'
    RUN_LABEL = 'Enlarge'
    CONSENT_KEY = 'consent.enlarge'
    CONSENT_TITLE = 'Enlarge images with AI on this computer?'
    CONSENT_LINES = (
        'Runs entirely on this computer. The image is never uploaded.',
        'The result is saved as a new derivative; the original is not changed.',
    )
    OPTIONS = ('scale',)
    DEFAULTS = {'scale': '3'}

    def options_nodes(self):
        return [ui.segmented('scale', [('2', '2x'), ('3', '3x')], str(self.opts['scale']))]

    def scale(self):
        return int(self.opts['scale'])

    def validate(self):
        problem = super().validate()
        if problem:
            return problem
        if self.scale() not in (2, 3):
            return 'Choose 2x or 3x.'
        if self.target['w'] * self.target['h'] * self.scale() ** 2 > ui.MAX_OUTPUT_PIXELS:
            return 'The enlarged image would exceed 40 megapixels; choose 2x.'
        return None

    def build_request(self, target, opts, rects, job):
        return {'version': 1, 'operation': 'enlarge', 'scale': int(opts['scale']),
                'max_pixels': ui.MAX_OUTPUT_PIXELS}


def main():
    return ui.run_app(Enlarge, HERE)
