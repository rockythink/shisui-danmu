"""Import-safe loopback Bilibili account adapter for isolated desktop QA."""
import collections
import http.cookies
import http.server
import json
import threading
import urllib.parse


class AccountDriver:
    def __init__(self):
        self.lock = threading.Lock()
        self.requests = []
        self.failures = []
        self.next_uids = collections.deque(('11', '22'))
        self.challenges = {}
        self.hold_logins = False
        self.expired_uids = set()
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'

            def log_message(self, *args):
                pass

            def send_json(self, value, *, cookies=(), status=200):
                data = json.dumps(value).encode()
                self.send_response(status)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(data)))
                self.send_header('Connection', 'close')
                for cookie in cookies:
                    self.send_header('Set-Cookie', cookie)
                self.close_connection = True
                self.end_headers()
                self.wfile.write(data)

            def record(self):
                parsed = urllib.parse.urlsplit(self.path)
                with owner.lock:
                    owner.requests.append((self.command, parsed.path))
                return parsed.path, urllib.parse.parse_qs(parsed.query)

            def uid(self):
                cookies = http.cookies.SimpleCookie(self.headers.get('Cookie', ''))
                value = cookies.get('DedeUserID')
                return value.value if value else None

            def do_GET(self):
                try:
                    path, query = self.record()
                    if path == '/x/passport-login/web/qrcode/generate':
                        with owner.lock:
                            uid = owner.next_uids.popleft() if owner.next_uids else '11'
                            key = 'qa-challenge-' + str(len(owner.challenges) + 1)
                            owner.challenges[key] = (uid, 0)
                        self.send_json({'code': 0, 'data': {'qrcode_key': key, 'url': owner.origin + '/qa-login/' + key}})
                    elif path == '/x/passport-login/web/qrcode/poll':
                        key = query['qrcode_key'][0]
                        with owner.lock:
                            uid, polls = owner.challenges[key]
                            owner.challenges[key] = (uid, polls + 1)
                            waiting = owner.hold_logins or polls == 0
                        if waiting:
                            self.send_json({'code': 0, 'data': {'code': 86101}})
                        else:
                            self.send_json({'code': 0, 'data': {'code': 0}}, cookies=(
                                'DedeUserID=' + uid + '; Path=/', 'bili_jct=qa-csrf-' + uid + '; Path=/',
                                'SESSDATA=qa-session-' + uid + '; Path=/'))
                    elif path == '/x/web-interface/nav':
                        uid = self.uid()
                        valid = uid in ('11', '22') and uid not in owner.expired_uids
                        self.send_json({'code': 0, 'data': {'isLogin': valid, 'mid': int(uid) if valid else 0,
                            'uname': 'QA Main' if uid == '11' else 'QA Assistant'}})
                    elif path == '/room/v1/Room/room_init':
                        self.send_json({'code': 0, 'data': {'room_id': 123, 'uid': 11, 'live_status': 1}})
                    elif path.endswith('/getOnlineRank'):
                        self.send_json({'code': 0, 'data': {'onlineNum': 8}})
                    else:
                        self.send_json({'code': -404, 'message': 'Synthetic QA route not implemented'}, status=404)
                except Exception as error:
                    with owner.lock:
                        owner.failures.append(type(error).__name__ + ': ' + str(error))
                    self.send_json({'code': -500}, status=500)

            def do_POST(self):
                try:
                    path, _ = self.record()
                    length = int(self.headers.get('Content-Length', '0'))
                    if length > 3 * 1024 * 1024:
                        self.send_json({'code': -400}, status=413)
                        return
                    body = self.rfile.read(length)
                    uid = self.uid()
                    if uid not in ('11', '22') or uid in owner.expired_uids:
                        self.send_json({'code': -101, 'message': 'Synthetic account expired'})
                    elif path == '/x/upload/web/image':
                        self.send_json({'code': 0, 'data': {'location': 'https://i0.hdslb.com/bfs/live/qa-fixture.png'}})
                    elif path in ('/room/v1/Room/update', '/xlive/app-blink/v1/preLive/UpdatePreLiveInfo', '/msg/send'):
                        form = urllib.parse.parse_qs(body.decode())
                        if form.get('csrf') != ['qa-csrf-' + uid]:
                            raise ValueError('synthetic CSRF binding mismatch')
                        self.send_json({'code': 0, 'data': {'audit_info': {'audit_title_status': 1,
                            'audit_title_reason': '合成待审核回执，不代表已展示'}}})
                    else:
                        self.send_json({'code': -404}, status=404)
                except Exception as error:
                    with owner.lock:
                        owner.failures.append(type(error).__name__ + ': ' + str(error))
                    self.send_json({'code': -500}, status=500)

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def origin(self):
        return 'http://127.0.0.1:' + str(self.server.server_port)

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(5)
