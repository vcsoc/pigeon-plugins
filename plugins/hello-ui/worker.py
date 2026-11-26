#!/usr/bin/env python3
"""hello-ui entry point. `--plugin` runs the UI protocol; everything else is the standalone runtime
(setup, health, version, self-test). Hello UI has no one-shot operations."""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)


def main():
    if "--plugin" in sys.argv:
        import app
        return app.run_plugin()
    import plugin_runtime
    plugin_runtime.main({})


if __name__ == "__main__":
    main()
