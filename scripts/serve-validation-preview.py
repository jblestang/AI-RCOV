#!/usr/bin/env python3
import argparse
import http.server
import json
import pathlib
import re
import urllib.error
import urllib.parse
import urllib.request


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--directory", type=pathlib.Path, required=True)
    args = parser.parse_args()
    directory = args.directory.resolve()
    preview = (directory / "preview.html").read_text(encoding="utf-8")
    match = re.search(r"sampleEndpoint='([^']+)'", preview)
    if match is None:
        raise SystemExit("sampleEndpoint absent; relancez scripts/validate-e2e.sh")
    endpoint = match.group(1)
    parsed = urllib.parse.urlsplit(endpoint)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise SystemExit("sampleEndpoint invalide")
    health_url = urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, "/health", "", ""))
    try:
        with urllib.request.urlopen(health_url, timeout=3) as response:
            if response.status != 200:
                raise SystemExit(f"API indisponible: {health_url} répond HTTP {response.status}")
    except (urllib.error.URLError, TimeoutError) as error:
        raise SystemExit(f"API indisponible sur {health_url}: {error}") from error

    class Handler(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *handler_args, **handler_kwargs):
            super().__init__(*handler_args, directory=str(directory), **handler_kwargs)

        def do_GET(self):
            request = urllib.parse.urlsplit(self.path)
            if request.path != "/__radial_sample":
                return super().do_GET()
            query = urllib.parse.parse_qs(request.query)
            if not all(query.get(name, [""])[0].isdigit() for name in ("col", "row")):
                return self.reply(400, {"error": "invalid sample coordinates"})
            target = endpoint + "?" + urllib.parse.urlencode(
                {name: query[name][0] for name in ("col", "row")}
            )
            try:
                with urllib.request.urlopen(target, timeout=10) as response:
                    body = response.read()
                    self.send_response(response.status)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
            except urllib.error.HTTPError as error:
                self.reply(error.code, {"error": error.read().decode("utf-8", "replace")})
            except (urllib.error.URLError, TimeoutError) as error:
                self.reply(502, {"error": f"API altitude inaccessible: {error}"})

        def reply(self, status: int, value: dict) -> None:
            body = json.dumps(value).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    http.server.ThreadingHTTPServer(("127.0.0.1", args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
