#!/usr/bin/env python3
"""Small mutable HTTP fixture for ESI compatibility tests."""

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--log", type=Path, required=True)
    parser.add_argument("--port-file", type=Path, required=True)
    args = parser.parse_args()
    args.root = args.root.resolve()

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            relative = unquote(urlsplit(self.path).path).lstrip("/")
            parts = Path(relative).parts
            with args.log.open("a", encoding="utf-8") as log:
                json.dump(
                    {
                        "path": urlsplit(self.path).path,
                        "headers": {key.lower(): value for key, value in self.headers.items()},
                    },
                    log,
                    sort_keys=True,
                )
                log.write("\n")

            if not relative or ".." in parts:
                self.send_error(400)
                return

            path = args.root.joinpath(*parts)
            status_path = Path(str(path) + ".status")
            if status_path.exists():
                status = int(status_path.read_text(encoding="ascii").strip())
                self.send_response(status)
                self.send_header("Content-Length", "0")
                self.end_headers()
                return

            if not path.is_file():
                self.send_response(404)
                self.send_header("Content-Length", "0")
                self.end_headers()
                return

            data = path.read_bytes()
            self.send_response(200)
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Content-Type", "application/octet-stream")
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, format: str, *args: object) -> None:
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    args.port_file.write_text(str(server.server_port), encoding="ascii")
    server.serve_forever()


if __name__ == "__main__":
    main()
