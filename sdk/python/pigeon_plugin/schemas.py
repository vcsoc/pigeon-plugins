"""Validate protocol v2 messages against schema/ui-v2 (development and test aid).

Needs the optional `jsonschema` package (and `referencing`, which it pulls in). The runtime never
imports this module, so plugins stay standard-library only.
"""
import json
from functools import lru_cache
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
SCHEMA_DIR = ROOT / "schema" / "ui-v2"
BASE = "https://pigeon.app/schema/ui-v2/"


def schema_dir():
    return SCHEMA_DIR


@lru_cache(maxsize=None)
def _registry():
    from referencing import Registry, Resource
    from referencing.jsonschema import DRAFT202012
    resources = []
    for path in sorted(SCHEMA_DIR.glob("*.json")):
        if path.name == "icons.json":
            continue
        doc = json.loads(path.read_text(encoding="utf-8"))
        resources.append((doc["$id"], Resource.from_contents(doc, default_specification=DRAFT202012)))
    return Registry().with_resources(resources)


@lru_cache(maxsize=None)
def _load(name):
    from jsonschema import Draft202012Validator
    doc = json.loads((SCHEMA_DIR / name).read_text(encoding="utf-8"))
    return Draft202012Validator(doc, registry=_registry())


def validate(message, schema="to_host.schema.json", pointer=None):
    """Raise jsonschema.ValidationError when `message` does not conform.

    `schema` is a file name in schema/ui-v2; `pointer` selects a $defs entry (for example
    "methods.schema.json" with pointer "library.query.params").
    """
    validator = _load(schema)
    target = message
    if pointer:
        from jsonschema import Draft202012Validator
        doc = json.loads((SCHEMA_DIR / schema).read_text(encoding="utf-8"))
        sub = doc["$defs"][pointer]
        Draft202012Validator(sub, registry=_registry()).validate(target)
        return
    validator.validate(target)


def is_valid(message, schema="to_host.schema.json", pointer=None):
    from jsonschema import ValidationError
    try:
        validate(message, schema, pointer)
        return True
    except ValidationError:
        return False


def method_params_schema(method):
    return "methods.schema.json", f"{method}.params"
