"""Windows-only, local libmpv smoke test. No network, no account, no GUI claims."""
import argparse
import ctypes as C
import hashlib
import json
import os
from pathlib import Path
import struct
import tempfile
import time
import wave


def validate_pe(path):
    with path.open('rb') as f:
        head = f.read(64)
        if len(head) != 64 or head[:2] != b'MZ':
            raise ValueError('Missing MZ header')
        offset = struct.unpack_from('<I', head, 60)[0]
        if not 64 <= offset <= min(1024 * 1024, path.stat().st_size - 26):
            raise ValueError('Invalid PE offset')
        f.seek(offset)
        pe = f.read(26)
        if len(pe) != 26 or pe[:4] != b'PE\0\0' or pe[4:6] != b'\x64\x86':
            raise ValueError('Expected x64 PE')
        if not (struct.unpack_from('<H', pe, 22)[0] & 0x2000):
            raise ValueError('Expected DLL')
        if pe[24:26] != b'\x0b\x02':
            raise ValueError('Expected PE32+')
    return True


def write_fixture(path):
    with wave.open(str(path), 'wb') as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(16000)
        wav.writeframes(b'\0\0' * 16000)


class Event(C.Structure):
    _fields_ = [('event_id', C.c_int), ('error', C.c_int),
                ('reply_userdata', C.c_uint64), ('data', C.c_void_p)]


class EndFile(C.Structure):
    _fields_ = [('reason', C.c_int), ('error', C.c_int)]


def probe(path):
    if os.name != 'nt':
        raise RuntimeError('Windows required for native runtime probe')
    path = path.resolve(strict=True)
    validate_pe(path)
    # Search only the explicitly selected DLL directory and default trusted dirs.
    # No current-working-directory DLL search.
    lib = C.CDLL(str(path), winmode=0x1100)
    def bind(name, ret, *args):
        f = getattr(lib, name)
        f.restype = ret
        f.argtypes = list(args)
        return f
    create = bind('mpv_create', C.c_void_p)
    init = bind('mpv_initialize', C.c_int, C.c_void_p)
    opt = bind('mpv_set_option_string', C.c_int, C.c_void_p, C.c_char_p, C.c_char_p)
    command = bind('mpv_command', C.c_int, C.c_void_p, C.POINTER(C.c_char_p))
    wait = bind('mpv_wait_event', C.POINTER(Event), C.c_void_p, C.c_double)
    destroy = bind('mpv_terminate_destroy', None, C.c_void_p)
    version = bind('mpv_client_api_version', C.c_ulong)()
    def check(code, operation):
        if code < 0:
            raise RuntimeError(f'{operation}: libmpv error {code}')
    handle = create()
    if not handle:
        raise RuntimeError('mpv_create failed')
    loaded = ended = False
    try:
        for key, value in [(b'config', b'no'), (b'terminal', b'no'), (b'idle', b'yes'),
                           (b'vo', b'null'), (b'ao', b'null'), (b'load-scripts', b'no')]:
            check(opt(handle, key, value), key.decode())
        check(init(handle), 'mpv_initialize')
        with tempfile.TemporaryDirectory(prefix='wetube-probe-') as directory:
            fixture = Path(directory) / 'local.wav'
            write_fixture(fixture)
            argv = (C.c_char_p * 3)(b'loadfile', str(fixture).encode('utf-8'), None)
            check(command(handle, argv), 'loadfile')
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                e = wait(handle, 0.25).contents
                if e.event_id == 8:  # MPV_EVENT_FILE_LOADED
                    loaded = True
                if e.event_id == 7:  # MPV_EVENT_END_FILE
                    if not e.data:
                        raise RuntimeError('Missing end-file data')
                    end = C.cast(e.data, C.POINTER(EndFile)).contents
                    if end.reason != 0 or end.error < 0:
                        raise RuntimeError(f'Playback failed: reason={end.reason}, error={end.error}')
                    ended = True
                    break
            if not loaded or not ended:
                raise RuntimeError('Local playback did not complete')
    finally:
        destroy(handle)
    return {'status': 'PASS', 'client_api_version': version, 'dll_sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
            'local_wav_loaded': loaded, 'local_wav_eof': ended,
            'scope': 'libmpv local audio with null output; not GPUI or YouTube runtime'}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('dll', type=Path)
    args = parser.parse_args()
    print(json.dumps(probe(args.dll), indent=2))
