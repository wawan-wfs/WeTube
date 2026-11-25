import tempfile
from pathlib import Path
import struct
import unittest
import wave
from runtime_probe import validate_pe, write_fixture

class Tests(unittest.TestCase):
    def test_valid_pe(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/'test.dll'
            b=bytearray(128); b[:2]=b'MZ'; struct.pack_into('<I',b,60,64); b[64:68]=b'PE\0\0'; struct.pack_into('<H',b,68,0x8664); struct.pack_into('<H',b,86,0x2000); struct.pack_into('<H',b,88,0x20b); p.write_bytes(b)
            self.assertTrue(validate_pe(p))
            for pos,value in [(68,0x14c),(86,0),(88,0x10b)]:
                x=bytearray(b);struct.pack_into('<H',x,pos,value);p.write_bytes(x)
                with self.assertRaises(ValueError):validate_pe(p)
    def test_bad_offset(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/'test.dll'; b=bytearray(64); b[:2]=b'MZ'; struct.pack_into('<I',b,60,4096);p.write_bytes(b)
            with self.assertRaises(ValueError):validate_pe(p)
    def test_truncated(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/'test.dll';p.write_bytes(b'MZ')
            with self.assertRaises(ValueError):validate_pe(p)
    def test_wav(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/'test.wav';write_fixture(p)
            with wave.open(str(p)) as w:
                self.assertEqual((w.getnchannels(),w.getsampwidth(),w.getframerate(),w.getnframes()),(1,2,16000,16000))
                self.assertEqual(w.readframes(16000),b'\0\0'*16000)

if __name__=='__main__':unittest.main(verbosity=2)
