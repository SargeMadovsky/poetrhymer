"""Sign local release assets; needs `pip install cryptography`.
Private seed is read from a path, never printed or included in output artifacts.
"""
import argparse
import hashlib
import json
from pathlib import Path
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

parser = argparse.ArgumentParser()
parser.add_argument("--seed", required=True, type=Path)
parser.add_argument("--version", required=True)
parser.add_argument("--dist", type=Path, default=Path("dist"))
args = parser.parse_args()
key = Ed25519PrivateKey.from_private_bytes(args.seed.read_bytes())
public = key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw).hex()
expected = Path(__file__).with_name("update-public-key.hex").read_text().strip()
if public != expected:
    raise SystemExit("Signing key does not match the public key embedded in the application")
binary = args.dist / "Poetrhymer.exe"
manifest = {
    "version": args.version,
    "target": "x86_64-pc-windows-msvc",
    "url": f"https://github.com/SargeMadovsky/poetrhymer/releases/download/v{args.version}/Poetrhymer.exe",
    "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
    "size": binary.stat().st_size,
}
data = (json.dumps(manifest, indent=2) + "\n").encode()
(args.dist / "update.json").write_bytes(data)
(args.dist / "update.json.sig").write_bytes(key.sign(data))
checksums = []
for asset in sorted(args.dist.glob("*.exe")):
    content = asset.read_bytes()
    signature = key.sign(content)
    key.public_key().verify(signature, content)
    (asset.with_name(asset.name + ".sig")).write_bytes(signature)
    checksums.append(f"{hashlib.sha256(content).hexdigest()}  {asset.name}")
(args.dist / "SHA256SUMS.txt").write_text("\n".join(checksums) + "\n", encoding="utf-8")
print("Release manifest and executable signatures generated and verified.")
