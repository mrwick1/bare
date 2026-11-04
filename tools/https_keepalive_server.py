"""Loopback HTTPS/1.1 keep-alive server for crates/net/examples/https_load.rs. Not for production.

Usage: python3 -I tools/https_keepalive_server.py CERT_PEM KEY_PEM PORT

It binds 127.0.0.1 only and needs a certificate and key that the caller made (the load tool makes
them in /dev/shm and deletes them). TLS 1.3 only. Routes:
  GET  /small        -> 200, body "ok"
  GET  /big/<mib>    -> 200, <mib> MiB of the pattern below, with Content-Length
  GET  /chunked/<mib>-> 200, the same bytes with Transfer-Encoding: chunked
  GET  /redirect     -> 302 to /small
  POST /sum          -> 200, body "<length> <sum of bytes mod 2^32>"
"""

import http.server
import ssl
import sys

BLOCK = 64 * 1024
# The pattern: byte i of a block is (i * 7 >> 3) & 255. The client rebuilds the same bytes.
PATTERN = bytes(((i * 7) >> 3) & 255 for i in range(BLOCK))


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    # The server answers with a head and then a body: Nagle would hold the body until our ACK (40 ms).
    disable_nagle_algorithm = True

    def log_message(self, *args):
        pass

    def reply(self, code, body, extra=()):
        self.send_response(code)
        for k, v in extra:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        p = self.path
        if p == "/small":
            self.reply(200, b"ok")
        elif p == "/redirect":
            self.reply(302, b"", [("Location", "/small")])
        elif p.startswith("/big/"):
            blocks = int(p[5:]) * (1024 * 1024 // BLOCK)
            self.send_response(200)
            self.send_header("Content-Length", str(blocks * BLOCK))
            self.end_headers()
            for _ in range(blocks):
                self.wfile.write(PATTERN)
        elif p.startswith("/chunked/"):
            blocks = int(p[9:]) * (1024 * 1024 // BLOCK)
            self.send_response(200)
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            for _ in range(blocks):
                self.wfile.write(b"%x\r\n" % BLOCK + PATTERN + b"\r\n")
            self.wfile.write(b"0\r\n\r\n")
        else:
            self.reply(404, b"no")

    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        total = 0
        left = n
        while left:
            data = self.rfile.read(min(left, BLOCK))
            if not data:
                break
            total = (total + sum(data)) & 0xFFFFFFFF
            left -= len(data)
        self.reply(200, f"{n} {total}".encode())


def main():
    cert, key, port = sys.argv[1], sys.argv[2], int(sys.argv[3])
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_3
    ctx.load_cert_chain(cert, key)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.socket = ctx.wrap_socket(server.socket, server_side=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
