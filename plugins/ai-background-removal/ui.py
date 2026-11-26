"""AI Background Removal: protocol v2 UI (inspector panel, asset action and tool window).

Runs in the plugin's UI process (standard library only). The model runs in a child process through
this folder's `run` launcher (see pigeon_ui.run_child).
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import ui_support as ui  # noqa: E402


class RemoveBackground(ui.ImageTool):
    OPERATION = 'remove_background'
    ACTION_ID = 'open'
    TITLE = 'AI Background Removal'
    RUN_LABEL = 'Remove background'
    CONSENT_KEY = 'consent.remove_background'
    CONSENT_TITLE = 'Remove backgrounds with AI on this computer?'
    CONSENT_LINES = (
        'Runs entirely on this computer with the U2-Net-P model. The image is never uploaded.',
        'Accepting saves a new PNG with a transparent background; the original is not changed.',
    )

    def build_request(self, target, opts, rects, job):
        return {'version': 1, 'operation': 'remove_background', 'max_pixels': ui.MAX_OUTPUT_PIXELS}


def main():
    return ui.run_app(RemoveBackground, HERE)
