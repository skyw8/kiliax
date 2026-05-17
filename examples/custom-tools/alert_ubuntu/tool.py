#!/usr/bin/env python3
import json
import shutil
import subprocess
import sys


def respond(call_id, result=None, error=None):
    payload = {"jsonrpc": "2.0", "id": call_id}
    if error is None:
        payload["result"] = {"content": result or ""}
    else:
        payload["error"] = {"message": error}
    print(json.dumps(payload), flush=True)


def run_command(argv):
    try:
        return subprocess.run(
            argv,
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            timeout=5,
        )
    except Exception as exc:
        return exc


def play_sound():
    candidates = [
        ["canberra-gtk-play", "--id", "message"],
        ["canberra-gtk-play", "--id", "bell"],
        ["paplay", "/usr/share/sounds/freedesktop/stereo/message.oga"],
        ["paplay", "/usr/share/sounds/freedesktop/stereo/complete.oga"],
        ["paplay", "/usr/share/sounds/ubuntu/notifications/Xylo.ogg"],
    ]
    for argv in candidates:
        if shutil.which(argv[0]) is None:
            continue
        result = run_command(argv)
        if isinstance(result, subprocess.CompletedProcess) and result.returncode == 0:
            return True
    return False


def handle(params):
    args = params.get("arguments") or {}
    title = str(args.get("title") or "Kiliax")
    message = str(args.get("message") or "").strip()
    urgency = str(args.get("urgency") or "normal")
    ring = bool(args.get("ring", True))

    if not message:
        raise ValueError("message must not be empty")
    if urgency not in {"low", "normal", "critical"}:
        raise ValueError("urgency must be low, normal, or critical")
    if shutil.which("notify-send") is None:
        raise RuntimeError("notify-send is not installed")

    result = run_command(["notify-send", "--urgency", urgency, title, message])
    if not isinstance(result, subprocess.CompletedProcess):
        raise RuntimeError(f"failed to run notify-send: {result}")
    if result.returncode != 0:
        detail = (result.stderr or "").strip()
        raise RuntimeError(detail or f"notify-send exited with {result.returncode}")

    sounded = play_sound() if ring else False
    suffix = " and played a sound" if sounded else ""
    return f"Sent Ubuntu notification{suffix}."


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        req = None
        try:
            req = json.loads(line)
            if req.get("method") != "call":
                raise ValueError("unsupported method")
            respond(req.get("id"), result=handle(req.get("params") or {}))
        except Exception as exc:
            respond(req.get("id") if isinstance(req, dict) else None, error=str(exc))


if __name__ == "__main__":
    main()
