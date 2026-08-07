"""Private stdio adapter for the Rust-owned desktop HTTP host.

This process deliberately does not import Uvicorn or bind a socket.  Rust
Actix-Web owns the public HTTP interface; this adapter preserves the existing
Python domain modules while they are migrated one route group at a time.
"""

from __future__ import annotations

import asyncio
import base64
import json
import multiprocessing
import sys
from typing import Any
from urllib.parse import urlsplit

from backend.app.main import create_app


async def invoke(app: Any, request: dict[str, Any]) -> dict[str, Any]:
    parsed = urlsplit(request["path_and_query"])
    raw_body = base64.b64decode(request.get("body_base64", ""))
    headers = [
        (str(name).encode("latin-1"), str(value).encode("latin-1"))
        for name, value in request.get("headers", [])
    ]
    scope = {
        "type": "http",
        "asgi": {"version": "3.0", "spec_version": "2.3"},
        "http_version": "1.1",
        "scheme": "http",
        "method": request["method"],
        "path": parsed.path or "/",
        "raw_path": (parsed.path or "/").encode("utf-8"),
        "query_string": parsed.query.encode("utf-8"),
        "root_path": "",
        "headers": headers,
        "client": ("127.0.0.1", 0),
        "server": ("127.0.0.1", 0),
    }
    sent_request = False
    status = 500
    response_headers: list[tuple[str, str]] = []
    chunks: list[bytes] = []

    async def receive() -> dict[str, Any]:
        nonlocal sent_request
        if sent_request:
            return {"type": "http.disconnect"}
        sent_request = True
        return {"type": "http.request", "body": raw_body, "more_body": False}

    async def send(message: dict[str, Any]) -> None:
        nonlocal status, response_headers
        if message["type"] == "http.response.start":
            status = message["status"]
            response_headers = [
                (name.decode("latin-1"), value.decode("latin-1"))
                for name, value in message.get("headers", [])
            ]
        elif message["type"] == "http.response.body":
            chunks.append(message.get("body", b""))

    await app(scope, receive, send)
    return {
        "id": request["id"],
        "status": status,
        "headers": response_headers,
        "body_base64": base64.b64encode(b"".join(chunks)).decode("ascii"),
    }


async def serve() -> None:
    app = create_app()
    write_lock = asyncio.Lock()

    async def write(payload: dict[str, Any]) -> None:
        encoded = json.dumps(payload, ensure_ascii=False, separators=(",", ":"))
        async with write_lock:
            await asyncio.to_thread(sys.stdout.write, encoded + "\n")
            await asyncio.to_thread(sys.stdout.flush)

    async with app.router.lifespan_context(app):
        await write({"kind": "ready"})
        tasks: set[asyncio.Task[None]] = set()

        async def handle(payload: dict[str, Any]) -> None:
            try:
                await write(await invoke(app, payload))
            except Exception as error:  # pragma: no cover - defence for protocol integrity
                await write(
                    {
                        "id": payload.get("id", 0),
                        "status": 500,
                        "headers": [("content-type", "application/json; charset=utf-8")],
                        "body_base64": base64.b64encode(
                            json.dumps(
                                {"error": {"code": "PYTHON_ADAPTER_ERROR", "message": str(error)}},
                                ensure_ascii=False,
                            ).encode("utf-8")
                        ).decode("ascii"),
                    }
                )

        while line := await asyncio.to_thread(sys.stdin.buffer.readline):
            try:
                payload = json.loads(line)
            except json.JSONDecodeError:
                continue
            task = asyncio.create_task(handle(payload))
            tasks.add(task)
            task.add_done_callback(tasks.discard)
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)


def main() -> None:
    multiprocessing.freeze_support()
    asyncio.run(serve())


if __name__ == "__main__":
    main()
