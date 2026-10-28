"""Exercise the compiled CLI against a real local HTTP server (stdlib only)."""
import argparse
import base64
import hashlib
import http.server
import json
import pathlib
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request
import xmlrpc.client

DATA = bytes(range(256)) * 4096


class Handler(http.server.BaseHTTPRequestHandler):
    ranges = []

    def log_message(self, *_):
        pass

    def do_GET(self):
        if self.path == "/digest":
            auth = self.headers.get("Authorization", "")
            valid = False
            if auth.startswith("Digest "):
                fields = urllib.request.parse_keqv_list(urllib.request.parse_http_list(auth[7:]))
                ha1 = hashlib.md5(b"user:ariai-smoke:pass").hexdigest()
                ha2 = hashlib.md5(b"GET:/digest").hexdigest()
                if fields.get("qop") == "auth":
                    expected = hashlib.md5(f'{ha1}:smoke-nonce:{fields.get("nc")}:{fields.get("cnonce")}:auth:{ha2}'.encode()).hexdigest()
                    valid = fields.get("response") == expected and fields.get("uri") == "/digest"
            if not valid:
                self.send_response(401)
                self.send_header("WWW-Authenticate", 'Digest realm="ariai-smoke", nonce="smoke-nonce", algorithm=MD5, qop="auth"')
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
        if self.path == "/cookie" and "smoke=yes" not in self.headers.get("Cookie", ""):
            self.send_error(403)
            return
        if self.path == "/remote.meta4":
            xml = f'<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="remote-metalink.bin"><size>{len(DATA)}</size><hash type="sha-256">{hashlib.sha256(DATA).hexdigest()}</hash><url>/file</url></file></metalink>'.encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(xml)))
            self.end_headers()
            self.wfile.write(xml)
            return
        if self.path == "/empty":
            ranged = self.headers.get("Range") is not None
            self.send_response(416 if ranged else 200)
            if ranged:
                self.send_header("Content-Range", "bytes */0")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if self.path == "/fail":
            self.send_error(503)
            return
        if self.path == "/auth" and (
            self.headers.get("Authorization") != "Basic dXNlcjpwYXNz"
            or self.headers.get("X-Ariai-Test") != "smoke"
        ):
            self.send_error(401)
            return
        if self.path == "/redirect":
            self.send_response(302)
            self.send_header("Location", "/file")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        first, last = 0, len(DATA) - 1
        ranged = self.headers.get("Range") and self.path != "/norange"
        if ranged:
            first_text, last_text = self.headers["Range"].removeprefix("bytes=").split("-")
            first = int(first_text)
            last = int(last_text) if last_text else last
            if first >= len(DATA) or last >= len(DATA) or last < first:
                self.send_response(416)
                self.send_header("Content-Range", f"bytes */{len(DATA)}")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            type(self).ranges.append((self.path, first, last))
        self.send_response(206 if ranged else 200)
        self.send_header("ETag", '"ariai-smoke-v1"')
        if self.path == "/set-cookie":
            self.send_header("Set-Cookie", "smoke=yes; Path=/; Max-Age=3600")
        self.send_header("Content-Length", str(last - first + 1))
        if ranged:
            reported_first = first + 1 if self.path == "/bad-range" else first
            self.send_header("Content-Range", f"bytes {reported_first}-{last}/{len(DATA)}")
        self.end_headers()
        try:
            self.wfile.write(DATA[first : last + 1])
        except (ConnectionResetError, BrokenPipeError, ConnectionAbortedError):
            pass


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=pathlib.Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    checks = []
    with tempfile.TemporaryDirectory(prefix="ariai-smoke-") as folder:
        directory = pathlib.Path(folder) / "中文 downloads"
        directory.mkdir()
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        base = f"http://127.0.0.1:{server.server_port}"

        def run(*arguments, success=True):
            result = subprocess.run(
                [str(binary), "--max-tries=1", "--retry-wait=0", "-d", str(directory), *arguments],
                text=True, encoding="utf-8", errors="replace", capture_output=True, timeout=30,
            )
            assert (result.returncode == 0) == success, (arguments, result.returncode, result.stderr, result.stdout)
            return result

        try:
            digest = hashlib.sha256(DATA).hexdigest()
            run("--split=4", "--min-split-size=64K", f"--checksum=sha-256={digest}", "-o", "segmented.bin", base + "/file")
            assert (directory / "segmented.bin").read_bytes() == DATA
            assert len(Handler.ranges) >= 3, Handler.ranges
            checks.append("segmented-checksum-unicode-path")

            run("--split=4", "--min-split-size=64K", "-o", "fallback.bin", base + "/norange")
            assert (directory / "fallback.bin").read_bytes() == DATA
            checks.append("server-ignores-range")

            run("--http-user=user", "--http-passwd=pass", "--header=X-Ariai-Test: smoke", "-o", "auth.bin", base + "/auth")
            assert (directory / "auth.bin").read_bytes() == DATA
            checks.append("http-basic-auth-custom-header")

            run("--http-user=user", "--http-passwd=pass", "--http-auth-challenge", "-o", "digest.bin", base + "/digest")
            assert (directory / "digest.bin").read_bytes() == DATA
            checks.append("http-digest-challenge")
            cookie_file = directory / "cookies.txt"
            run(f"--save-cookies={cookie_file}", "-o", "cookie-set.bin", base + "/set-cookie")
            assert "smoke" in cookie_file.read_text(encoding="utf-8")
            run(f"--load-cookies={cookie_file}", "-o", "cookie.bin", base + "/cookie")
            assert (directory / "cookie.bin").read_bytes() == DATA
            checks.append("netscape-cookie-save-load")

            run("-o", "mirror.bin", base + "/fail", base + "/file")
            assert (directory / "mirror.bin").read_bytes() == DATA
            checks.append("mirror-failover")

            run("-o", "redirect.bin", base + "/redirect")
            assert (directory / "redirect.bin").read_bytes() == DATA
            checks.append("redirect")

            run("--force-sequential", "--max-concurrent-downloads=2", base + "/sequential-a", base + "/sequential-b")
            assert (directory / "sequential-a").read_bytes() == DATA
            assert (directory / "sequential-b").read_bytes() == DATA
            checks.append("force-sequential-multiple-jobs")

            run("--checksum=sha-256=" + "0" * 64, "-o", "corrupt.bin", base + "/file", success=False)
            assert not (directory / "corrupt.bin").exists(), "checksum failure published a final output"
            checks.append("checksum-failure-is-not-complete")

            run("-o", "empty.bin", base + "/empty")
            assert (directory / "empty.bin").read_bytes() == b""
            checks.append("zero-length-file")

            run("-o", "bad-range.bin", base + "/bad-range", success=False)
            assert not (directory / "bad-range.bin").exists()
            checks.append("invalid-content-range-rejected")

            run("gopher://127.0.0.1/not-implemented", success=False)
            run("--this-option-is-not-implemented=true", base + "/file", success=False)
            checks.append("unsupported-protocol-option-rejected")

            xml = f'<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="local-metalink.bin"><size>{len(DATA)}</size><hash type="sha-256">{digest}</hash><url>{base}/file</url></file></metalink>'
            metadata = directory / "files.meta4"
            metadata.write_text(xml, encoding="utf-8")
            run("-M", str(metadata), "--select-file=1")
            assert (directory / "local-metalink.bin").read_bytes() == DATA
            checks.append("cli-metalink-file-checksum")
            run(base + "/remote.meta4")
            assert (directory / "remote-metalink.bin").read_bytes() == DATA
            checks.append("cli-metalink-http-relative-url")

            piece_size = len(DATA) // 4
            piece_xml = "".join(f'<hash>{hashlib.sha256(DATA[n:n+piece_size]).hexdigest()}</hash>' for n in range(0, len(DATA), piece_size))
            repair_xml = f'<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="repair.bin"><size>{len(DATA)}</size><pieces type="sha-256" length="{piece_size}">{piece_xml}</pieces><url>{base}/file</url></file></metalink>'
            repair_meta = directory / "repair.meta4"
            repair_meta.write_text(repair_xml, encoding="utf-8")
            damaged = bytearray(DATA)
            damaged[piece_size + 1] ^= 255
            (directory / "repair.bin").write_bytes(damaged)
            Handler.ranges.clear()
            run("-M", str(repair_meta), "--check-integrity")
            assert (directory / "repair.bin").read_bytes() == DATA
            assert Handler.ranges == [("/file", piece_size, 2 * piece_size - 1)], Handler.ranges
            checks.append("metalink-selective-piece-repair")

            (directory / "seed.bin").write_bytes(DATA)
            torrent = directory / "fixture.torrent"
            torrent.write_bytes(b"d4:infod6:lengthi1048576e4:name8:seed.bin12:piece lengthi1048576e6:pieces20:" + hashlib.sha1(DATA).digest() + b"ee")
            result = run("-T", str(torrent), "--continue", "--enable-dht=false", "--json")
            snapshot = json.loads(result.stdout)[0]
            assert snapshot["status"] == "complete", snapshot
            assert snapshot["files"][0]["completed_length"] == len(DATA)
            assert (directory / "seed.bin").read_bytes() == DATA
            checks.append("cli-torrent-existing-data-verification")

            with socket.socket() as candidate:
                candidate.bind(("127.0.0.1", 0))
                rpc_port = candidate.getsockname()[1]
            session_file = directory / "queue.json"
            process = subprocess.Popen(
                [str(binary), "--enable-rpc", f"--rpc-listen-port={rpc_port}", "--rpc-secret=smoke-secret", f"--save-session={session_file}"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )

            def rpc(method, params):
                body = json.dumps({"jsonrpc": "2.0", "id": "smoke", "method": method, "params": ["token:smoke-secret", *params]}).encode()
                request = urllib.request.Request(f"http://127.0.0.1:{rpc_port}/jsonrpc", data=body, headers={"Content-Type": "application/json"})
                with urllib.request.urlopen(request, timeout=5) as response:
                    value = json.load(response)
                assert "error" not in value, value
                return value["result"]

            try:
                for _ in range(100):
                    try:
                        methods = rpc("system.listMethods", [])
                        break
                    except OSError:
                        assert process.poll() is None, "RPC process exited during startup"
                        time.sleep(0.1)
                else:
                    raise AssertionError("RPC startup timed out")
                assert "aria2.addUri" in methods
                with xmlrpc.client.ServerProxy(f"http://127.0.0.1:{rpc_port}/rpc") as client:
                    version = client.aria2.getVersion("token:smoke-secret")
                    assert version["version"] == "0.3.0", version
                    results = client.system.multicall([
                        {"methodName": "aria2.getVersion", "params": ["token:smoke-secret"]},
                        {"methodName": "aria2.getVersion", "params": ["token:wrong"]},
                    ])
                    assert results[0][0]["version"] == "0.3.0" and "faultCode" in results[1], results
                checks.append("xmlrpc-token-multicall")
                gid = rpc("aria2.addUri", [[base + "/file"], {"dir": str(directory), "out": "rpc.bin"}])
                for _ in range(100):
                    status = rpc("aria2.tellStatus", [gid])
                    if status["status"] == "complete":
                        break
                    assert status["status"] != "error", status
                    time.sleep(0.1)
                else:
                    raise AssertionError("RPC download timed out")
                assert (directory / "rpc.bin").read_bytes() == DATA
                assert status["completedLength"] == str(len(DATA))
                xml = xml.replace("local-metalink.bin", "rpc-metalink.bin")
                meta_gid = rpc("aria2.addMetalink", [base64.b64encode(xml.encode()).decode(), {"dir": str(directory)}])[0]
                for _ in range(100):
                    status = rpc("aria2.tellStatus", [meta_gid])
                    if status["status"] == "complete":
                        break
                    assert status["status"] != "error", status
                    time.sleep(0.1)
                else:
                    raise AssertionError("RPC Metalink timed out")
                assert (directory / "rpc-metalink.bin").read_bytes() == DATA
                assert rpc("aria2.getFiles", [meta_gid])[0]["completedLength"] == str(len(DATA))
                checks.append("rpc-metalink-submission-files")
                assert rpc("aria2.saveSession", []) == "OK"
                json.loads(session_file.read_text(encoding="utf-8"))
                assert rpc("aria2.shutdown", []) == "OK"
                output, error = process.communicate(timeout=15)
                assert process.returncode == 0, (output, error)
                checks.append("rpc-process-download-session-shutdown")
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.communicate(timeout=5)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)
    print(json.dumps({"result": "PASS", "checks": checks}, ensure_ascii=False))


if __name__ == "__main__":
    main()

