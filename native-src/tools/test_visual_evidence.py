"""Synthetic validator fixtures, NOT actual Windows captures."""
import copy
import hashlib
from pathlib import Path
import struct
import tempfile
import unittest
import zlib
from visual_evidence import validate


def chunk(kind, payload):
    return struct.pack('>I', len(payload))+kind+payload+struct.pack('>I', zlib.crc32(kind+payload))


def image(w,h):
    return (b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',w,h,8,0,0,0,0))+
            chunk(b'IDAT',zlib.compress((b'\0'+b'\x80'*w)*h))+chunk(b'IEND',b''))


class VisualEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name)
        self.path=self.root/'Home-720-100.png';self.raw=image(720,720);self.path.write_bytes(self.raw)
        state={'schema':1,'page':'Home','requested_width':720,'requested_height':720,'requested_scale':1,'viewport_width':720,'viewport_height':720,'ui_scale':1,'os_scale':1,'root_focused':True,'search_focused':False}
        trace=[state,dict(state,root_focused=False,search_focused=True),state]
        self.row={'page':'Home','width':720,'height':720,'scale':1,'status':'CAPTURED','focus':'PASS','focus_trace':trace,'visual_review':'NOT REVIEWED','screenshot':self.path.name,'sha256':hashlib.sha256(self.raw).hexdigest(),'dpi':96,'pixel_width':720,'pixel_height':720,'state':state}
        self.report={'schema':1,'exe_sha256':'a'*64,'required_dpi':96,'cells':[self.row]}
    def tearDown(self): self.tmp.cleanup()
    def invalid(self,change):
        r=copy.deepcopy(self.report);change(r);self.assertTrue(validate(self.root,r)['errors'])
    def test_valid_fixture_is_not_visual_pass(self):
        r=validate(self.root,self.report);self.assertEqual(r['captured'],1);self.assertEqual(r['visual_review_pass'],0);self.assertEqual(r['missing'],159);self.assertEqual(r['focus_pass'],1)
    def test_wrong_route(self):self.invalid(lambda r:r['cells'][0]['state'].update(page='Watch'))
    def test_wrong_dpi(self):self.invalid(lambda r:r['cells'][0].update(dpi=120))
    def test_wrong_digest(self):self.invalid(lambda r:r['cells'][0].update(sha256='b'*64))
    def test_duplicate(self):self.invalid(lambda r:r['cells'].append(r['cells'][0]))
    def test_path_escape(self):self.invalid(lambda r:r['cells'][0].update(screenshot='../Home-720-100.png'))
    def test_claimed_review(self):self.invalid(lambda r:r['cells'][0].update(visual_review='PASS'))
    def test_focus_trace(self):self.invalid(lambda r:r['cells'][0].update(focus_trace=[]))
    def test_invalid_compressed_pixels_even_with_valid_crc(self):
        raw=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',720,720,8,0,0,0,0))+chunk(b'IDAT',b'not zlib')+chunk(b'IEND',b'')
        self.path.write_bytes(raw);self.invalid(lambda r:r['cells'][0].update(sha256=hashlib.sha256(raw).hexdigest()))
    def test_short_scanline_payload(self):
        raw=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',720,720,8,0,0,0,0))+chunk(b'IDAT',zlib.compress(b'\0'*50))+chunk(b'IEND',b'')
        self.path.write_bytes(raw);self.invalid(lambda r:r['cells'][0].update(sha256=hashlib.sha256(raw).hexdigest()))
    def test_crc_even_with_recomputed_digest(self):
        raw=self.raw[:-1]+bytes([self.raw[-1]^1]);self.path.write_bytes(raw)
        self.invalid(lambda r:r['cells'][0].update(sha256=hashlib.sha256(raw).hexdigest()))

if __name__=='__main__':unittest.main()
