"""AI Image Expand (MI-GAN): protocol v2 UI. Margins in pixels; the new border is painted by the model.

Runs in the plugin's UI process (standard library only); the model runs in a child process.
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import ui_support as ui  # noqa: E402


class Expand(ui.ExpandTool):
    OPERATION = 'expand'
    ACTION_ID = 'open'
    TITLE = 'AI Image Expand'
    RUN_LABEL = 'Expand canvas'
    CONSENT_KEY = 'consent.expand'
    CONSENT_TITLE = 'Expand canvases with AI on this computer?'
    CONSENT_LINES = (
        'Runs entirely on this computer with the MI-GAN model. The image is never uploaded.',
        'Your original pixels are kept exactly; only the new border is generated. Accepting saves a derivative.',
    )


def main():
    return ui.run_app(Expand, HERE)
