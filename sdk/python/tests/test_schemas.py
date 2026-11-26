"""Every JSON Schema compiles; every valid fixture conforms; every invalid fixture is rejected."""
import json
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve()
sys.path.insert(0, str(HERE.parents[1]))

from pigeon_plugin import schemas  # noqa: E402
from jsonschema import Draft202012Validator, ValidationError  # noqa: E402

EXAMPLES = schemas.SCHEMA_DIR / "examples"


def target_for(name):
    """valid/<direction>__<msg>.json -> (schema file, root)."""
    if name.startswith("manifest_ui"):
        return "manifest-ui.schema.json"
    if name.startswith("to_host__"):
        return "to_host.schema.json"
    if name.startswith("to_plugin__"):
        return "to_plugin.schema.json"
    raise AssertionError(f"no target schema for fixture {name}")


class SchemaTests(unittest.TestCase):
    def test_every_schema_file_is_a_valid_2020_12_schema(self):
        for path in sorted(schemas.SCHEMA_DIR.glob("*.json")):
            if path.name == "icons.json":
                continue
            with self.subTest(schema=path.name):
                Draft202012Validator.check_schema(json.loads(path.read_text(encoding="utf-8")))

    def test_icon_list_is_sorted_unique_kebab_case(self):
        icons = json.loads((schemas.SCHEMA_DIR / "icons.json").read_text())["names"]
        self.assertEqual(icons, sorted(set(icons)))
        for name in icons:
            self.assertRegex(name, r"^[a-z0-9][a-z0-9-]*$")
        self.assertGreaterEqual(len(icons), 150)

    def test_valid_fixtures_conform(self):
        files = sorted((EXAMPLES / "valid").glob("*.json"))
        self.assertGreaterEqual(len(files), 40)
        for path in files:
            with self.subTest(fixture=path.name):
                doc = json.loads(path.read_text(encoding="utf-8"))
                schemas.validate(doc, target_for(path.name))

    def test_invalid_fixtures_are_rejected(self):
        files = sorted((EXAMPLES / "invalid").glob("*.json"))
        self.assertGreaterEqual(len(files), 15)
        for path in files:
            with self.subTest(fixture=path.name):
                doc = json.loads(path.read_text(encoding="utf-8"))
                with self.assertRaises(ValidationError):
                    schemas.validate(doc, target_for(path.name))

    def test_all_kinds_scene_covers_every_node_kind(self):
        schema = json.loads((schemas.SCHEMA_DIR / "scene.schema.json").read_text())
        kinds = set(schema["$defs"]["node"]["properties"]["kind"]["enum"])
        doc = json.loads((EXAMPLES / "valid" / "to_host__ui_set_all_kinds.json").read_text())
        seen = set()

        def walk(node):
            seen.add(node["kind"])
            for c in node.get("children", []):
                walk(c)
        walk(doc["scene"])
        self.assertEqual(kinds - seen, set(), "node kinds not exercised by the all-kinds fixture")

    def test_every_method_has_params_and_result_defs(self):
        doc = json.loads((schemas.SCHEMA_DIR / "methods.schema.json").read_text())
        methods = {k.rsplit(".", 1)[0] for k in doc["$defs"]}
        for m in methods:
            self.assertIn(m + ".params", doc["$defs"])
            self.assertIn(m + ".result", doc["$defs"])
        for required in ["selection.get", "library.query", "library.import", "secrets.get", "dialog.open_file",
                         "shell.open_url", "notify.toast", "progress.start", "window.open_modal", "drag.begin",
                         "theme.get", "log", "source.list", "source.thumbnail", "source.upload", "search.query"]:
            self.assertIn(required, methods)

    def test_method_params_validate_and_reject(self):
        schemas.validate({"mode_id": "semantic", "query": "x"}, "methods.schema.json", "search.query.params")
        with self.assertRaises(ValidationError):
            schemas.validate({"mode_id": "semantic", "query": 5}, "methods.schema.json", "search.query.params")
        schemas.validate({"url": "https://example.com"}, "methods.schema.json", "shell.open_url.params")
        with self.assertRaises(ValidationError):
            schemas.validate({"url": "file:///etc/passwd"}, "methods.schema.json", "shell.open_url.params")

    def test_every_slot_in_the_protocol_is_in_the_contribution_schema(self):
        schema = json.loads((schemas.SCHEMA_DIR / "contribution.schema.json").read_text())
        slots = set(schema["properties"]["slot"]["enum"])
        expected = {"sidebar.section", "sidebar.entry", "toolbar.item", "workspace.view", "inspector.panel",
                    "preferences.page", "modal", "statusbar.item", "asset.action", "library.source",
                    "search.provider", "drop.target", "command"}
        self.assertEqual(slots, expected)


if __name__ == "__main__":
    unittest.main()
