import sys
base, patch, out = sys.argv[1], sys.argv[2], sys.argv[3]
data = bytearray(open(base,'rb').read())
p = open(patch,'rb').read()
assert p[:5]==b'PATCH', "not an IPS"
i=5
while True:
    if p[i:i+3]==b'EOF': break
    off=int.from_bytes(p[i:i+3],'big'); i+=3
    size=int.from_bytes(p[i:i+2],'big'); i+=2
    if size==0:
        rl=int.from_bytes(p[i:i+2],'big'); i+=2
        val=p[i]; i+=1
        if off+rl>len(data): data.extend(b'\x00'*(off+rl-len(data)))
        for k in range(rl): data[off+k]=val
    else:
        chunk=p[i:i+size]; i+=size
        if off+size>len(data): data.extend(b'\x00'*(off+size-len(data)))
        data[off:off+size]=chunk
open(out,'wb').write(data)
print(f"wrote {out}: {len(data)} bytes")
