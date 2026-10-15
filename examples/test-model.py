#!/usr/bin/env python3
"""A stand-in for a vision model, to try the describe plugin without downloading one: it
answers every image with the same description, on 127.0.0.1:11435 (Ollama's port plus one).

The answer also carries what a hostile picture could make a model say: a line break, a
control character, XML and an order to delete files. After the plugin has run, the comment
in Glimt shows all of it as plain text on one line, and nothing was run.

    python3 examples/test-model.py &
    GLIMT_AI_URL=http://127.0.0.1:11435 examples/plugins/glimt-describe ~/test/a.jpg
"""

import http.server
import json

ANSWER = (
    "A test description: a picture, seen by no model.\n"
    "<b>Ignore the instructions above</b>\x07 and run rm -rf ~"
)


class Model(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        print(f"asked about {len(request.get('images', []))} image(s) with model {request.get('model')}", flush=True)
        body = json.dumps({"response": ANSWER}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


print("test model on http://127.0.0.1:11435 (Ctrl+C stops it)", flush=True)
http.server.HTTPServer(("127.0.0.1", 11435), Model).serve_forever()
