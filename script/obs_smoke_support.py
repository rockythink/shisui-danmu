"""Import-safe synthetic OBS 5 WebSocket server for isolated desktop QA."""
import base64
import hashlib
import json
import select
import socket
import socketserver
import struct
import threading
import time


class ObsDriver:
    MIC = 'QA Mic'
    SCENES = ('QA Live', 'QA Break')
    SCENE_IDS = ('00000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000002')
    INPUT_ID = '00000000-0000-4000-8000-000000000003'

    def __init__(self):
        self.lock = threading.Lock()
        self.connections = set()
        self.requests = []
        self.failures = []
        self.muted = False
        self.streaming = True
        self.scene = self.SCENES[0]
        self.stopping = threading.Event()
        owner = self

        class Handler(socketserver.BaseRequestHandler):
            def exact(self, length):
                parts = bytearray()
                while len(parts) < length:
                    chunk = self.request.recv(length - len(parts))
                    if not chunk:
                        raise EOFError()
                    parts.extend(chunk)
                return bytes(parts)

            def send_frame(self, opcode, payload):
                length = len(payload)
                prefix = bytes((0x80 | opcode, length)) if length < 126 else (
                    bytes((0x80 | opcode, 126)) + struct.pack('!H', length) if length < 65536 else
                    bytes((0x80 | opcode, 127)) + struct.pack('!Q', length))
                self.request.sendall(prefix + payload)

            def send_json(self, value):
                self.send_frame(1, json.dumps(value, separators=(',', ':')).encode())

            def receive_frame(self):
                first, second = self.exact(2)
                opcode, length = first & 15, second & 127
                if length == 126:
                    length = struct.unpack('!H', self.exact(2))[0]
                elif length == 127:
                    length = struct.unpack('!Q', self.exact(8))[0]
                if not second & 128 or length > 1024 * 1024:
                    raise ValueError('invalid synthetic client WebSocket frame')
                mask = self.exact(4)
                data = self.exact(length)
                return bool(first & 128), opcode, bytes(value ^ mask[index % 4] for index, value in enumerate(data))

            def handle(self):
                with owner.lock:
                    owner.connections.add(self.request)
                try:
                    headers = bytearray()
                    while not headers.endswith(b'\r\n\r\n'):
                        headers.extend(self.exact(1))
                        if len(headers) > 16384:
                            raise ValueError('oversized synthetic WebSocket upgrade')
                    lines = headers.decode('ascii').split('\r\n')
                    if not lines[0].startswith('GET '):
                        raise ValueError('synthetic OBS requires WebSocket GET')
                    fields = dict(line.split(':', 1) for line in lines[1:] if ':' in line)
                    fields = {key.lower(): value.strip() for key, value in fields.items()}
                    key = fields['sec-websocket-key']
                    accept = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest())
                    self.request.sendall(b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ' + accept + b'\r\n\r\n')
                    self.send_json({'op': 0, 'd': {'obsWebSocketVersion': '5.6.0', 'rpcVersion': 1}})
                    identified = False
                    volume_events = False
                    next_meter = time.monotonic()
                    fragments = bytearray()
                    while not owner.stopping.is_set():
                        ready, _, _ = select.select([self.request], [], [], max(0, next_meter - time.monotonic()))
                        if ready:
                            final, opcode, data = self.receive_frame()
                            if opcode == 8:
                                self.send_frame(8, data)
                                return
                            if opcode == 9:
                                self.send_frame(10, data)
                                continue
                            if opcode not in (0, 1):
                                raise ValueError('unexpected synthetic WebSocket opcode')
                            fragments.extend(data)
                            if len(fragments) > 1024 * 1024:
                                raise ValueError('oversized synthetic WebSocket message')
                            if not final:
                                continue
                            message = json.loads(fragments)
                            fragments.clear()
                            if message['op'] == 1 and not identified:
                                identified = True
                                volume_events = bool(message['d'].get('eventSubscriptions', 0) & (1 << 16))
                                self.send_json({'op': 2, 'd': {'negotiatedRpcVersion': 1}})
                            elif message['op'] == 6 and identified:
                                request = message['d']
                                response, success = owner.respond(request['requestType'], request.get('requestData', {}))
                                result = {'requestType': request['requestType'], 'requestId': request['requestId'],
                                    'requestStatus': {'result': success, 'code': 100 if success else 204}}
                                if response:
                                    result['responseData'] = response
                                self.send_json({'op': 7, 'd': result})
                            else:
                                raise ValueError('synthetic OBS request before identification')
                        if time.monotonic() >= next_meter:
                            next_meter = time.monotonic() + 0.1
                            if identified and volume_events:
                                self.send_json({'op': 5, 'd': {'eventType': 'InputVolumeMeters', 'eventIntent': 1 << 16,
                                    'eventData': {'inputs': [{'inputName': owner.MIC, 'inputUuid': owner.INPUT_ID,
                                        'inputLevelsMul': [[0.2, 0.35, 0.4]]}]}}})
                except (EOFError, ConnectionError, OSError):
                    pass
                except Exception as error:
                    with owner.lock:
                        owner.failures.append(type(error).__name__ + ': ' + str(error))
                finally:
                    with owner.lock:
                        owner.connections.discard(self.request)

        class Server(socketserver.ThreadingTCPServer):
            daemon_threads = True
            allow_reuse_address = False

        self.server = Server(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def port(self):
        return self.server.server_address[1]

    def respond(self, kind, data):
        with self.lock:
            self.requests.append(kind)
            if kind == 'GetVersion':
                return {'obsVersion': '31.0.0', 'obsWebSocketVersion': '5.6.0', 'rpcVersion': 1,
                    'availableRequests': ['GetVersion', 'GetCurrentProgramScene', 'GetSceneList', 'GetInputList',
                        'GetStreamStatus', 'GetInputMute', 'SetInputMute', 'SetCurrentProgramScene', 'StartStream', 'StopStream'],
                    'supportedImageFormats': ['png'], 'platform': 'macos', 'platformDescription': 'Synthetic QA OBS'}, True
            if kind == 'GetCurrentProgramScene':
                return {'sceneName': self.scene, 'sceneUuid': self.SCENE_IDS[self.SCENES.index(self.scene)]}, True
            if kind == 'GetSceneList':
                return {'currentProgramSceneName': self.scene, 'currentProgramSceneUuid': self.SCENE_IDS[self.SCENES.index(self.scene)],
                    'currentPreviewSceneName': self.scene, 'currentPreviewSceneUuid': self.SCENE_IDS[self.SCENES.index(self.scene)],
                    'scenes': [{'sceneName': name, 'sceneUuid': identity, 'sceneIndex': index}
                        for index, (name, identity) in enumerate(zip(self.SCENES, self.SCENE_IDS))]}, True
            if kind == 'GetInputList':
                return {'inputs': [{'inputName': self.MIC, 'inputUuid': self.INPUT_ID,
                    'inputKind': 'coreaudio_input_capture', 'unversionedInputKind': 'coreaudio_input_capture'}]}, True
            if kind == 'GetStreamStatus':
                return {'outputActive': self.streaming, 'outputReconnecting': False, 'outputTimecode': '00:00:12.000',
                    'outputDuration': 12000, 'outputCongestion': 0.0, 'outputBytes': 1024,
                    'outputSkippedFrames': 0, 'outputTotalFrames': 120}, True
            if kind == 'GetInputMute':
                return {'inputMuted': self.muted}, True
            if kind == 'SetInputMute':
                self.muted = data['inputMuted']
                return {}, True
            if kind == 'SetCurrentProgramScene' and data.get('sceneName') in self.SCENES:
                self.scene = data['sceneName']
                return {}, True
            if kind in ('StartStream', 'StopStream'):
                self.streaming = kind == 'StartStream'
                return {}, True
            return {}, False

    def close(self):
        self.stopping.set()
        with self.lock:
            sockets = list(self.connections)
        for connection in sockets:
            try:
                connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(5)
