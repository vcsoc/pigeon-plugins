"""AI Image Expand (Diffusion): protocol v2 UI for Stable Diffusion 1.5 inpainting.

Options: an optional scene prompt, manual steps and guidance (off by default: the engine picks values
for the device it finds), a time budget (the engine uses a smaller working size when the estimate
exceeds it; 0 means no limit), and an optional fixed seed. The device the engine reports is shown in
the panel. Generation is slow (minutes for large images) and always runs in a child process.
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import ui_support as ui  # noqa: E402

MAX_PROMPT = 500


class ExpandDiffusion(ui.ExpandTool):
    OPERATION = 'expand'
    ACTION_ID = 'open'
    TITLE = 'AI Image Expand (Diffusion)'
    RUN_LABEL = 'Expand canvas'
    CONSENT_KEY = 'consent.expand-diffusion'
    CONSENT_TITLE = 'Generate with diffusion AI on this computer?'
    CONSENT_LINES = (
        'Runs entirely on this computer with Stable Diffusion 1.5 inpainting. Images are never uploaded.',
        'It is slow: minutes for large images, longer on a CPU. Your original pixels are kept exactly.',
    )
    OPTIONS = ('left', 'top', 'right', 'bottom', 'prompt', 'manual', 'steps', 'guidance',
               'time_budget', 'fixed_seed', 'seed')
    DEFAULTS = {'left': 0, 'top': 256, 'right': 0, 'bottom': 256, 'prompt': '', 'manual': False,
                'steps': 12, 'guidance': 1.0, 'time_budget': 45, 'fixed_seed': False, 'seed': 1}

    def options_nodes(self):
        nodes = list(super().options_nodes())
        nodes.append(ui.text_input('prompt', self.opts['prompt'], self.revs.get('prompt', 0),
                                   placeholder='Describe the scene (optional)', max_len=MAX_PROMPT))
        nodes.append(ui.number_input('time_budget', self.opts['time_budget'], self.revs.get('time_budget', 0),
                                     0, 3600, 5, 's', 0))
        nodes.append(ui.checkbox('manual', 'Set steps and guidance myself', bool(self.opts['manual'])))
        if self.opts['manual']:
            nodes.append(ui.number_input('steps', self.opts['steps'], self.revs.get('steps', 0), 1, 50, 1, None, 0))
            nodes.append(ui.slider('guidance', self.opts['guidance'], self.revs.get('guidance', 0), 1, 12, 0.5))
        nodes.append(ui.checkbox('fixed_seed', 'Use a fixed seed', bool(self.opts['fixed_seed'])))
        if self.opts['fixed_seed']:
            nodes.append(ui.number_input('seed', self.opts['seed'], self.revs.get('seed', 0), 0, 2147483647, 1,
                                         None, 0))
        return nodes

    def validate(self):
        problem = super().validate()
        if problem:
            return problem
        if len(str(self.opts['prompt'])) > MAX_PROMPT:
            return f'The prompt can be at most {MAX_PROMPT} characters.'
        try:
            budget = float(self.opts['time_budget'])
            if not 0 <= budget <= 3600:
                return 'The time budget must be between 0 and 3600 seconds (0 = no limit).'
            if self.opts['manual']:
                steps = int(self.opts['steps'])
                guidance = float(self.opts['guidance'])
                if not 1 <= steps <= 50:
                    return 'Steps must be between 1 and 50.'
                if not 1.0 <= guidance <= 12.0:
                    return 'Guidance must be between 1 and 12.'
        except (TypeError, ValueError):
            return 'Enter numbers for steps, guidance and time budget.'
        return None

    def build_request(self, target, opts, rects, job):
        request = super().build_request(target, opts, rects, job)
        if str(opts.get('prompt') or '').strip():
            request['prompt'] = str(opts['prompt']).strip()[:MAX_PROMPT]
        request['time_budget'] = float(opts['time_budget'])
        if opts.get('manual'):
            request['steps'] = int(opts['steps'])
            request['guidance'] = float(opts['guidance'])
        if opts.get('fixed_seed'):
            request['seed'] = int(opts['seed'])
        return request


def main():
    return ui.run_app(ExpandDiffusion, HERE)
