"""AI Object Removal: protocol v2 UI with a rectangle mask painted on a canvas.

The mask uses the same semantics as the previous built-in editor: rectangles in source-image pixels,
applied in order. Mark adds a rectangle (removed area), Erase takes part of a marked area back. At
most 256 rectangles; the rasterised mask must not be empty. The mask is written at the source size.
"""
import math
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import ui_support as ui  # noqa: E402

MAX_RECTANGLES = 256
CANVAS_BOX = (360, 300)
MARK = '#ff372d73'
ERASE = '#2ec4b633'


class RemoveObject(ui.ImageTool):
    OPERATION = 'remove_object'
    ACTION_ID = 'open'
    TITLE = 'AI Object Removal'
    RUN_LABEL = 'Remove object'
    CONSENT_KEY = 'consent.remove_object'
    CONSENT_TITLE = 'Remove objects with AI on this computer?'
    CONSENT_LINES = (
        'Runs entirely on this computer with the LaMa model. The image is never uploaded.',
        'Only the painted area changes. Accepting saves a new derivative; the original is kept.',
    )
    OPTIONS = ('mode',)
    DEFAULTS = {'mode': 'mark'}
    CANVAS_IDS = ('mask',)
    EXTRA_ACTIONS = {'clear': 'clear_mask'}

    def __init__(self, folder):
        super().__init__(folder)
        self.frame = 0
        self.rects = []
        self.drag = None

    def rects_reset(self):
        self.rects = []
        self.drag = None

    def snapshot_rects(self):
        return [dict(rect) for rect in self.rects]

    def canvas_size(self):
        return ui.fit_box(self.target['w'], self.target['h'], *CANVAS_BOX)

    def options_nodes(self):
        return [ui.segmented('mode', [('mark', 'Mark'), ('erase', 'Erase')], self.opts['mode'])]

    def extra_nodes(self):
        if not self.target or not self.target.get('w'):
            return []
        cw, ch = self.canvas_size()
        ops = [ui.draw_image(self.source_rid(), 0, 0, cw, ch)]
        for rect in self.rects:
            ops.append(self.canvas_rect(rect))
        if self.drag:
            x0, y0, x1, y1 = self.drag
            ops.append(ui.draw_rect(min(x0, x1), min(y0, y1), abs(x1 - x0), abs(y1 - y0),
                                    None, '#ffffff', 1.0))
        marked = sum(1 for rect in self.rects if not rect.get('erase'))
        status = (f'{marked} marked, {len(self.rects)} of {MAX_RECTANGLES} rectangles. '
                  'Red areas will be removed.')
        self.frame += 1
        return [
            ui.canvas('mask', cw, ch, ops, frame=self.frame,
                      interactive={'pointer': True, 'coalesce': 'move', 'focusable': False}),
            ui.text('mask-status', status, style='caption'),
            ui.button('clear', 'Clear mask', variant='ghost', enabled=bool(self.rects)),
        ]

    def canvas_rect(self, rect):
        cw, ch = self.canvas_size()
        w, h = self.target['w'], self.target['h']
        x0, y0 = rect['min']
        x1, y1 = rect['max']
        bounds = {'x': x0 * cw / w, 'y': y0 * ch / h, 'w': (x1 - x0) * cw / w, 'h': (y1 - y0) * ch / h}
        if rect.get('erase'):
            pw, ph = self.target['source_size']
            return ui.draw.image(self.source_rid(), dst=bounds, src={
                'x': x0 * pw / w, 'y': y0 * ph / h, 'w': (x1 - x0) * pw / w, 'h': (y1 - y0) * ph / h})
        return ui.draw_rect(**bounds, fill=MARK)

    def on_canvas(self, msg):
        if msg.get('type') != 'pointer' or not self.target or not self.target.get('w'):
            return
        cw, ch = self.canvas_size()
        x = min(max(float(msg.get('x', 0)), 0.0), float(cw))
        y = min(max(float(msg.get('y', 0)), 0.0), float(ch))
        phase = msg.get('phase')
        if phase == 'down':
            self.drag = [x, y, x, y]
        elif phase == 'move' and self.drag:
            self.drag[2], self.drag[3] = x, y
        elif phase == 'up' and self.drag:
            start = self.drag
            self.drag = None
            self.commit(start[0], start[1], x, y)
        elif phase == 'cancel':
            self.drag = None

    def commit(self, x0, y0, x1, y1):
        w, h = self.target['w'], self.target['h']
        cw, ch = self.canvas_size()
        left, right = sorted((x0, x1))
        top, bottom = sorted((y0, y1))
        min_x = max(0, math.floor(left * w / cw))
        min_y = max(0, math.floor(top * h / ch))
        max_x = min(w, math.ceil(right * w / cw))
        max_y = min(h, math.ceil(bottom * h / ch))
        if min_x >= max_x or min_y >= max_y:
            return                                  # too small to mean anything: ignored
        if len(self.rects) >= MAX_RECTANGLES:
            self.error = f'At most {MAX_RECTANGLES} rectangles. Clear the mask to start again.'
            return
        self.rects.append({'min': [min_x, min_y], 'max': [max_x, max_y],
                           'erase': self.opts['mode'] == 'erase'})
        self.error = ''

    def clear_mask(self):
        self.rects = []
        self.drag = None

    def validate(self):
        problem = super().validate()
        if problem:
            return problem
        if not any(not rect.get('erase') for rect in self.rects):
            return 'Drag rectangles over the object to remove.'
        return None

    def build_request(self, target, opts, rects, job):
        mask_path = os.path.join(self.session, 'results', f'{job}-mask.png')
        ui.write_mask_png(mask_path, target['w'], target['h'], rects)
        return {'version': 1, 'operation': 'remove_object', 'mask': mask_path,
                'max_pixels': ui.MAX_OUTPUT_PIXELS}


def main():
    return ui.run_app(RemoveObject, HERE)
