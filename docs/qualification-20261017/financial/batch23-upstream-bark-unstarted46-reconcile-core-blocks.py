"""Pure public Bitcoin byte decoders only; extracted from the original audit helper.
No paths, entrypoint, file I/O, node/RPC/DB calls, process actions, or execution on import.
"""
import hashlib
import struct

def digest(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()[::-1].hex()

def vi(b, p):
    n = b[p]
    p += 1
    if n < 253:
        return n, p
    size = {253: 2, 254: 4, 255: 8}[n]
    return int.from_bytes(b[p:p+size], 'little'), p+size

def tx(b, p):
    start = p
    version = b[p:p+4]
    p += 4
    witness = b[p:p+2] == b'\x00\x01'
    if witness:
        p += 2
    body_start = p
    count, p = vi(b, p)
    inputs = []
    for _ in range(count):
        prev = b[p:p+32][::-1].hex()
        index = struct.unpack_from('<I', b, p+32)[0]
        p += 36
        size, p = vi(b, p)
        p += size+4
        inputs.append((prev, index))
    count, p = vi(b, p)
    outputs = []
    for _ in range(count):
        value = struct.unpack_from('<Q', b, p)[0]
        p += 8
        size, p = vi(b, p)
        outputs.append((value, b[p:p+size]))
        p += size
    body_end = p
    if witness:
        for _ in inputs:
            count, p = vi(b, p)
            for _ in range(count):
                size, p = vi(b, p)
                p += size
    locktime = b[p:p+4]
    p += 4
    assert p <= len(b) and len(locktime) == 4
    return {'txid': digest(version+b[body_start:body_end]+locktime),
            'inputs': inputs, 'outputs': outputs, 'raw_size': p-start}, p
