#!/usr/bin/env python3
"""Write a private Core descriptor import from an authentic server seed phrase."""

import argparse
import hashlib
import hmac
import json
import os
from pathlib import Path
import stat
import sys
import unicodedata


PATHS = {"rounds": 1856555996, "watchman": 38644432}
BASE58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
INPUT_CHARSET = "0123456789()[],'/*abcdefgh@:$%{}IJKLMNOPQRSTUVWXYZ&+-.;<=>?!^_|~ijklmnopqrstuvwxyzABCDEFGH`#\"\\ "
CHECKSUM_CHARSET = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"


def descriptor_checksum(descriptor):
	"""BIP380 checksum. This does not derive keys or validate a descriptor."""
	def polymod(checksum, value):
		top = checksum >> 35
		checksum = ((checksum & 0x7ffffffff) << 5) ^ value
		for bit, generator in enumerate((
			0xf5dee51989, 0xa9fdca3312, 0x1bab10e32d, 0x3706b1677a, 0x644d626ffd,
		)):
			if (top >> bit) & 1:
				checksum ^= generator
		return checksum

	checksum, group, count = 1, 0, 0
	for character in descriptor:
		position = INPUT_CHARSET.index(character)
		checksum = polymod(checksum, position & 31)
		group = group * 3 + (position >> 5)
		count += 1
		if count == 3:
			checksum = polymod(checksum, group)
			group, count = 0, 0
	if count:
		checksum = polymod(checksum, group)
	for _ in range(8):
		checksum = polymod(checksum, 0)
	checksum ^= 1
	return "".join(CHECKSUM_CHARSET[(checksum >> (5 * (7 - i))) & 31] for i in range(8))


def master_tprv(phrase):
	# Server creation uses BIP39 with an empty passphrase. Core derives all
	# child keys; this helper only serializes the BIP32 master private key.
	seed = hashlib.pbkdf2_hmac("sha512", phrase.encode("utf-8"), b"mnemonic", 2048)
	key = hmac.digest(b"Bitcoin seed", seed, "sha512")
	if not 0 < int.from_bytes(key[:32], "big") < 0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141:
		raise ValueError("invalid BIP32 master key")
	payload = bytes.fromhex("04358394") + bytes(9) + key[32:] + b"\0" + key[:32]
	encoded = payload + hashlib.sha256(hashlib.sha256(payload).digest()).digest()[:4]
	number = int.from_bytes(encoded, "big")
	result = ""
	while number:
		number, remainder = divmod(number, 58)
		result = BASE58[remainder] + result
	return "1" * (len(encoded) - len(encoded.lstrip(b"\0"))) + result


def main():
	parser = argparse.ArgumentParser(description=__doc__)
	# Both supported chains use tprv. Refuse mainnet before reading the seed.
	parser.add_argument("--network", required=True, choices=("regtest", "signet"))
	parser.add_argument("--wallet", required=True, choices=PATHS)
	parser.add_argument("--range-end", required=True, type=int)
	parser.add_argument("--next-index", required=True, type=int)
	parser.add_argument("--output", required=True, type=Path)
	args = parser.parse_args()
	if not 0 <= args.next_index <= args.range_end < 2**31:
		parser.error("require 0 <= next-index <= range-end < 2^31")
	input_stat = os.fstat(sys.stdin.fileno())
	if not stat.S_ISREG(input_stat.st_mode) or input_stat.st_uid != os.getuid() or input_stat.st_mode & 0o077:
		parser.error("stdin must be a regular owner-only seed file owned by this user")
	parent_stat = args.output.parent.stat()
	if parent_stat.st_uid != os.getuid() or parent_stat.st_mode & 0o077:
		parser.error("output directory must be owner-only and owned by this user")
	phrase = unicodedata.normalize("NFKD", " ".join(sys.stdin.read(4096).split()))
	if len(phrase.split()) != 12 or any(not word.isascii() or not word.isalpha() or not word.islower() for word in phrase.split()):
		parser.error("require the original 12-word English server seed phrase")
	# Identity and BIP39 word/checksum validation remain the operator's
	# responsibility. A typo must fail the public funding-script comparison.
	descriptor = f"tr({master_tprv(phrase)}/{PATHS[args.wallet]}h/0/*)"
	request = [{
		"desc": descriptor + "#" + descriptor_checksum(descriptor),
		"timestamp": 0,
		"range": [0, args.range_end],
		"next_index": args.next_index,
		"active": True,
		"internal": False,
	}]
	# Never overwrite an earlier recovery input or print private descriptors.
	fd = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
	with os.fdopen(fd, "w") as output:
		json.dump(request, output)
		output.write("\n")


if __name__ == "__main__":
	try:
		main()
	except (OSError, ValueError):
		sys.exit("cannot create private descriptor import; check inputs and permissions locally")
