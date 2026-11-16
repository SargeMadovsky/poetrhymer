"""Verify that Windows PE resources contain every original ICO image.
Usage: python tools/verify_windows_icon.py path/to/app.exe
Uses only the Python standard library; does not execute the inspected file.
"""
import struct
import sys
from pathlib import Path

icon = (Path(__file__).resolve().parents[1] / "assets/icon/poetrhymer.ico").read_bytes()
count = struct.unpack_from("<H", icon, 4)[0]
expected = set()
for i in range(count):
    length, offset = struct.unpack_from("<II", icon, 6 + i * 16 + 8)
    expected.add(icon[offset:offset + length])

for filename in sys.argv[1:]:
    data = Path(filename).read_bytes()
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    assert data[pe:pe+4] == b"PE\0\0", "Not a PE file"
    sections = struct.unpack_from("<H", data, pe + 6)[0]
    optional_size = struct.unpack_from("<H", data, pe + 20)[0]
    optional = pe + 24
    magic = struct.unpack_from("<H", data, optional)[0]
    directories = optional + (112 if magic == 0x20B else 96)
    resource_rva = struct.unpack_from("<I", data, directories + 16)[0]

    def file_offset(rva):
        for index in range(sections):
            section = optional + optional_size + index * 40
            virtual_size, virtual_address, raw_size, raw_address = struct.unpack_from("<IIII", data, section + 8)
            if virtual_address <= rva < virtual_address + max(virtual_size, raw_size):
                return raw_address + rva - virtual_address
        raise ValueError("RVA is outside PE sections")

    root = file_offset(resource_rva)
    leaves = []

    def walk(offset, path=()):
        named, ids = struct.unpack_from("<HH", data, root + offset + 12)
        for index in range(named + ids):
            name, child = struct.unpack_from("<II", data, root + offset + 16 + index * 8)
            key = path + (name,)
            if child & 0x80000000:
                walk(child & 0x7FFFFFFF, key)
            else:
                rva, size = struct.unpack_from("<II", data, root + child)
                start = file_offset(rva)
                leaves.append((key, data[start:start + size]))

    walk(0)
    actual = {payload for path, payload in leaves if path[0] == 3}
    assert expected <= actual, f"{filename}: missing original icon images"
    assert any(path[0] == 14 for path, _ in leaves), "Missing icon group"
    assert any(path[0] == 16 for path, _ in leaves), "Missing version resource"
    print(f"PASS {filename}: {count} original icon sizes, icon group and version resource")
