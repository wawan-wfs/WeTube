"""Generate an original indexed AVI: 4 s of moving uncompressed RGB video, no network."""
import struct
import sys
from pathlib import Path

WIDTH, HEIGHT, FPS, COUNT = 160, 90, 10, 40
FRAME_SIZE = WIDTH * HEIGHT * 3


def chunk(tag, data):
    return tag + struct.pack('<I', len(data)) + data + b'\0' * (len(data) % 2)


def list_chunk(kind, data):
    return chunk(b'LIST', kind + data)


def make_avi():
    avih = struct.pack('<14I', 1000000 // FPS, FRAME_SIZE * FPS, 0, 0x10,
                       COUNT, 0, 1, FRAME_SIZE, WIDTH, HEIGHT, 0, 0, 0, 0)
    strh = struct.pack('<4s4sIHH8I4h', b'vids', b'DIB ', 0, 0, 0, 0, 1, FPS,
                       0, COUNT, FRAME_SIZE, 0xffffffff, 0, 0, 0, WIDTH, HEIGHT)
    strf = struct.pack('<IiiHHIIiiII', 40, WIDTH, HEIGHT, 1, 24, 0,
                       FRAME_SIZE, 0, 0, 0, 0)
    hdrl = list_chunk(b'hdrl', chunk(b'avih', avih) +
                      list_chunk(b'strl', chunk(b'strh', strh) + chunk(b'strf', strf)))
    frames, index, offset = [], [], 4
    for i in range(COUNT):
        # Uniform but different RGB values on every frame: deterministic motion in color.
        pixel = bytes((30 + i * 4, 210 - i * 4, 40 + i * 3))
        data = pixel * (WIDTH * HEIGHT)
        frame = chunk(b'00db', data)
        frames.append(frame)
        index.append(struct.pack('<4sIII', b'00db', 0x10, offset, len(data)))
        offset += len(frame)
    movi = list_chunk(b'movi', b''.join(frames))
    return chunk(b'RIFF', b'AVI ' + hdrl + movi + chunk(b'idx1', b''.join(index)))


if __name__ == '__main__':
    Path(sys.argv[1]).write_bytes(make_avi())
