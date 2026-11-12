#!/usr/bin/env python3
"""Print an ELF64 little-endian binary's DT_RUNPATH/DT_RPATH, without binutils."""

import struct
import sys


def fail(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(1)


if len(sys.argv) != 2:
    fail(f"usage: {sys.argv[0]} ELF")

data = open(sys.argv[1], "rb").read()
if data[:6] != b"\x7fELF\x02\x01" or len(data) < 64:
    fail("not a little-endian ELF64 file")

phoff = struct.unpack_from("<Q", data, 32)[0]
phentsize, phnum = struct.unpack_from("<HH", data, 54)
loads = []
dynamic = None
for index in range(phnum):
    at = phoff + index * phentsize
    if at + 56 > len(data):
        fail("truncated program header table")
    kind = struct.unpack_from("<I", data, at)[0]
    offset, vaddr, filesz = struct.unpack_from("<QQQ", data, at + 8)[0], struct.unpack_from("<Q", data, at + 16)[0], struct.unpack_from("<Q", data, at + 32)[0]
    if kind == 1:
        loads.append((offset, vaddr, filesz))
    elif kind == 2:
        dynamic = (offset, filesz)

if dynamic is None:
    fail("no PT_DYNAMIC segment")

strtab = None
runpath_index = None
rpath_index = None
offset, length = dynamic
for at in range(offset, offset + length, 16):
    if at + 16 > len(data):
        fail("truncated dynamic segment")
    tag, value = struct.unpack_from("<QQ", data, at)
    if tag == 0:
        break
    if tag == 5:
        strtab = value
    elif tag == 29:
        runpath_index = value
    elif tag == 15:
        rpath_index = value

index = runpath_index if runpath_index is not None else rpath_index
if strtab is None or index is None:
    fail("no DT_RUNPATH or DT_RPATH")

file_offset = None
for offset, vaddr, filesz in loads:
    if vaddr <= strtab < vaddr + filesz:
        file_offset = offset + strtab - vaddr + index
        break
if file_offset is None or file_offset >= len(data):
    fail("dynamic string table is not file-backed")

end = data.find(b"\0", file_offset)
if end < 0:
    fail("unterminated RUNPATH")
print(data[file_offset:end].decode("utf-8"))
