"""Validate capture evidence, never declare visual acceptance from metadata alone."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import struct
import sys
import zlib

PAGES = ('Home','Watch','Shorts','Search','Channel','Subscriptions','Library','Playlist','Downloads','Settings')
CELLS = {(p,w,s) for p in PAGES for w in (720,856,1280,1920) for s in (0.75,1,1.25,1.5)}
HEX = re.compile(r'^[0-9a-f]{64}$')


def require(ok, reason):
    if not ok:
        raise ValueError(reason)


def png_dimensions(raw):
    require(raw[:8] == b'\x89PNG\r\n\x1a\n', 'Not PNG')
    offset, dims, idat, ended = 8, None, False, False
    while offset + 12 <= len(raw):
        size, kind = struct.unpack_from('>I4s', raw, offset)
        require(size <= len(raw)-offset-12, 'Truncated PNG chunk')
        chunk = raw[offset+8:offset+8+size]
        crc = struct.unpack_from('>I', raw, offset+8+size)[0]
        require(zlib.crc32(kind+chunk) == crc, 'PNG CRC mismatch')
        if offset == 8:
            require(kind == b'IHDR' and size == 13, 'Missing IHDR')
            dims = struct.unpack_from('>II', chunk)
            require(0 < dims[0] <= 16384 and 0 < dims[1] <= 16384, 'Invalid dimensions')
        if kind == b'IDAT':
            idat = True
        offset += 12+size
        if kind == b'IEND':
            require(size == 0 and offset == len(raw), 'Invalid IEND')
            ended = True
            break
    require(dims and idat and ended, 'Incomplete PNG')
    return dims


def validate(root, report):
    require(report.get('schema') == 1 and HEX.fullmatch(report.get('exe_sha256','')), 'Invalid evidence schema/executable hash')
    seen = set()
    captured = focus_pass = focus_fail = 0
    errors = []
    rows = report.get('cells')
    require(isinstance(rows, list) and len(rows) <= len(CELLS), 'Invalid cell count')
    for row in rows:
        try:
            key = (row['page'], row['width'], row['scale'])
            require(key in CELLS and key not in seen, 'Unknown or duplicate cell')
            seen.add(key)
            require(row.get('visual_review') == 'NOT REVIEWED', 'Capture cannot certify visual review')
            if row['status'] == 'NOT TESTED':
                require(not row.get('screenshot') and not row.get('sha256'), 'Uncaptured cell has screenshot claim')
                continue
            require(row['status'] == 'CAPTURED', 'Unknown status')
            page,width,scale = key
            expected = f'{page}-{width}-{int(scale*100)}.png'
            require(row['screenshot'] == expected, 'Incorrect screenshot filename')
            path = root/expected
            require(path.is_file() and not path.is_symlink() and path.resolve().parent == root.resolve(), 'Unsafe or missing screenshot')
            require(path.stat().st_size <= 64*1024*1024, 'Screenshot too large')
            raw = path.read_bytes()
            require(hashlib.sha256(raw).hexdigest() == row['sha256'], 'Screenshot digest mismatch')
            w,h = png_dimensions(raw)
            require((w,h) == (row['pixel_width'],row['pixel_height']), 'PNG dimensions disagree')
            dpi = row['dpi']
            require(type(dpi) is int and 48 <= dpi <= 480, 'Invalid OS DPI')
            require(not report.get('required_dpi') or dpi == report['required_dpi'], 'Wrong requested OS DPI')
            s = row['state']
            require(s['schema']==1 and s['page']==page and s['requested_width']==width and s['requested_height']==row['height'] and abs(s['requested_scale']-scale)<0.001, 'Wrong rendered route or request')
            for v in (s['viewport_width'],s['viewport_height'],s['os_scale'],s['ui_scale']):
                require(isinstance(v,(int,float)) and math.isfinite(v) and v>0, 'Nonfinite measurement')
            require(abs(s['viewport_width']-width)<=2 and abs(s['viewport_height']-row['height'])<=2 and abs(s['ui_scale']-scale)<0.001 and abs(s['os_scale']-dpi/96)<0.02, 'Measured geometry or scale mismatch')
            require(abs(w-s['viewport_width']*dpi/96)<=4 and abs(h-s['viewport_height']*dpi/96)<=4, 'Physical pixel/viewport mismatch')
            require(row['focus'] in ('PASS','FAIL','NOT TESTED'), 'Unknown focus status')
            if row['focus']=='PASS':
                trace=row.get('focus_trace')
                require(isinstance(trace,list) and len(trace)==3,'Missing measured focus trace')
                for i,state in enumerate(trace):
                    require(state['page']==page and abs(state['viewport_width']-width)<=2 and abs(state['viewport_height']-row['height'])<=2 and abs(state['ui_scale']-scale)<0.001 and abs(state['os_scale']-dpi/96)<0.02,'Focus trace geometry mismatch')
                    require(state.get('root_focused') is (i!=1) and state.get('search_focused') is (i==1),'Focus trace did not alternate root/search/root')
            captured += 1
            focus_pass += row['focus']=='PASS'
            focus_fail += row['focus']=='FAIL'
        except (ValueError,KeyError,TypeError,OSError,struct.error) as e:
            errors.append({'cell':row.get('page','?') if isinstance(row,dict) else '?','reason':str(e)})
    return {'expected':len(CELLS),'recorded':len(seen),'captured':captured,'focus_pass':focus_pass,'focus_fail':focus_fail,'visual_review_pass':0,'missing':len(CELLS)-len(seen),'errors':errors,'status':'EVIDENCE VALID' if not errors else 'INVALID EVIDENCE','scope':'Integrity and metadata consistency only; no independent authentication of screenshot origin or visual quality.'}


def main():
    p=argparse.ArgumentParser()
    p.add_argument('directory',type=Path)
    p.add_argument('--require-all',action='store_true')
    a=p.parse_args()
    path=a.directory/'matrix.json'
    require(path.stat().st_size<=4*1024*1024,'Manifest too large')
    result=validate(a.directory,json.loads(path.read_text(encoding='utf-8-sig')))
    print(json.dumps(result,indent=2))
    return 1 if result['errors'] or (a.require_all and result['captured']!=len(CELLS)) else 0


if __name__=='__main__':
    sys.exit(main())
