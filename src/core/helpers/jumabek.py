"""The JumaBek skill protocol, for skills written in Python.

One JSON line arrives on stdin, one JSON line goes back on stdout. That is the
whole contract, and the only rule that matters: **stdout belongs to the
protocol**. A stray `print()` in the middle of a response corrupts the line the
core is parsing, so `run()` quietly points `sys.stdout` at stderr and keeps the
real one to itself. Print whatever you like; it lands in the log, not in the
protocol.

    import jumabek

    def execute(method, args):
        if method == "greet":
            return "hello, " + args
        raise jumabek.SkillError("no such method: " + method, kind="NotFound")

    jumabek.run(
        name="greeter",
        version="0.1.0",
        description="Says hello to whoever is named in the arguments",
        methods=[{
            "method": "greet",
            "description": "Greet someone by name",
            "args_description": "the name to greet, as plain text",
        }],
        execute=execute,
    )

`args` is always a string — the core does not interpret it. If your method takes
structured input, agree on JSON and parse it yourself.
"""

import json
import sys

__all__ = ["run", "SkillError", "text", "json_output", "empty"]

#: The error kinds the core understands. Anything else is sent as
#: ExecutionFailed, because an unknown kind would fail to deserialise and cost
#: the caller a turn.
KINDS = ("NotFound", "ExecutionFailed", "InvalidArgs", "Recoverable", "Fatal")


class SkillError(Exception):
    """Raise this to answer with an error instead of dying.

    A skill that crashes gets restarted and the caller learns nothing; a skill
    that answers with an error tells the model what to do differently.
    """

    def __init__(self, message, kind="ExecutionFailed"):
        super().__init__(message)
        self.message = str(message)
        self.kind = kind if kind in KINDS else "ExecutionFailed"


def text(value):
    """Answer with plain text."""
    return {"Text": str(value)}


def json_output(value):
    """Answer with structured data the model can pick apart."""
    return {"Json": value}


def empty():
    """Answer with nothing — the call worked and has no result."""
    return "Empty"


def _output(result):
    """Turn whatever a handler returned into an Output payload.

    Returning a bare string or a dict is the common case and is meant to just
    work; the explicit `text()` / `json_output()` helpers are for when the guess
    would be wrong.
    """
    if result is None:
        return {"Output": "Empty"}
    if result == "Empty":
        return {"Output": "Empty"}
    if isinstance(result, dict) and len(result) == 1:
        only = next(iter(result))
        if only in ("Text", "Json", "Binary"):
            return {"Output": result}
    if isinstance(result, (dict, list)):
        return {"Output": {"Json": result}}
    if isinstance(result, (bytes, bytearray)):
        return {"Output": {"Binary": list(result)}}
    return {"Output": {"Text": str(result)}}


def _execute(params, execute):
    if params is None:
        return {"Error": {"InvalidArgs": "Not provided any parameters"}}

    try:
        parsed = json.loads(params) if isinstance(params, str) else params
        method = parsed["method"]
        args = parsed.get("args") or ""
    except (ValueError, TypeError, KeyError) as error:
        return {"Error": {"InvalidArgs": str(error)}}

    try:
        return _output(execute(method, args))
    except SkillError as error:
        return {"Error": {error.kind: error.message}}
    except Exception as error:  # noqa: BLE001 - a skill must not die on bad input
        return {"Error": {"ExecutionFailed": "%s: %s" % (type(error).__name__, error)}}


def run(name, version, description, methods, execute, health_check=None):
    """Serve the protocol until stdin closes.

    `methods` is a list of dicts with `method`, `description` and
    `args_description`. The validator refuses a skill that leaves any of them
    empty, so describe them as if the reader has never seen the skill.
    """
    protocol = sys.stdout
    sys.stdout = sys.stderr

    metadata = {"name": name, "version": version, "description": description}

    def answer(identifier, payload):
        protocol.write(json.dumps({"id": identifier, "payload": payload}) + "\n")
        protocol.flush()

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        try:
            request = json.loads(line)
        except ValueError as error:
            answer(0, {"Error": {"InvalidArgs": str(error)}})
            continue

        identifier = request.get("id", 0)
        method = request.get("method", "")

        if method == "get_metadata":
            payload = {"Metadata": metadata}
        elif method == "available_methods":
            payload = {"Methods": methods}
        elif method == "health_check":
            payload = {"Health": bool(health_check()) if health_check else True}
        elif method == "execute":
            payload = _execute(request.get("params"), execute)
        else:
            payload = {"Error": {"NotFound": "Method not found"}}

        answer(identifier, payload)
