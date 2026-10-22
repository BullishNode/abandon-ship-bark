#!/usr/bin/env python3
"""Bundle qualified GNU programs and existing Nix runtime closures without a build."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile


PROGRAMS = ("bark", "barkd", "captaind", "watchmand")
PLUGINS = ("hold", "cln-grpc")
HASH = re.compile(r"[0-9a-f]{64}")
STORE_NAME = re.compile(r"[0-9a-z]{32}-[A-Za-z0-9+._?=-]+")
DOCUMENTS = {
	"docs/operator.md": "OPERATOR.md",
	"RELEASE_NOTES.md": "RELEASE_NOTES.md",
	"contrib/release/ROLLBACK.md": "ROLLBACK.md",
	"contrib/release/README.md": "BUNDLE.md",
	"server/captaind.default.toml": "captaind.default.toml",
	"server/watchmand.default.toml": "watchmand.default.toml",
	"contrib/release/server-seed-descriptors.py": "server-seed-descriptors.py",
}


def digest(path):
	with path.open("rb") as stream:
		return hashlib.file_digest(stream, "sha256").hexdigest()


def run(*args):
	return subprocess.check_output(args, text=True).strip()


def regular_file(root, name):
	"""Accept only regular files below the specified directory, without symlinks."""
	parts = PurePosixPath(name).parts
	if not parts or name != "/".join(parts) or any(p in (".", "..") for p in parts):
		raise ValueError("invalid relative file name")
	path = root
	for part in parts:
		path = path / part
		if path.is_symlink():
			raise ValueError(f"symlink input is not permitted: {name}")
	if not path.is_relative_to(root) or not path.is_file():
		raise ValueError(f"regular file required: {name}")
	return path


def store_root(path):
	path = path.resolve(strict=True)
	if path.parts[:3] != ("/", "nix", "store") or len(path.parts) < 4:
		raise ValueError("runtime inputs must be existing Nix store paths")
	root = Path(*path.parts[:4])
	if not STORE_NAME.fullmatch(root.name):
		raise ValueError("invalid Nix store path")
	return root


def checksum_file(path):
	checksums = {}
	for line in path.read_text().splitlines():
		value, name = line.split(maxsplit=1)
		name = name.removeprefix("*")
		parts = PurePosixPath(name).parts
		if (not HASH.fullmatch(value) or not parts or name.startswith("/")
			or name != "/".join(parts) or ".." in parts or name in checksums):
			raise ValueError("invalid or duplicate artifact checksum")
		checksums[name] = value
	return checksums


def unique_json_object(pairs):
	result = {}
	for name, value in pairs:
		if name in result:
			raise ValueError("duplicate JSON field")
		result[name] = value
	return result


def plugin_checksums(qualification):
	plugins = qualification["plugins"]
	if isinstance(plugins, list):
		pairs = [(item["name"], item["sha256"]) for item in plugins]
		plugins = unique_json_object(pairs)
	if not isinstance(plugins, dict):
		raise ValueError("plugins must be a checksum object or a name/sha256 list")
	result = {}
	for name in PLUGINS:
		value = plugins[name]
		if not isinstance(value, str) or not HASH.fullmatch(value):
			raise ValueError(f"invalid plugin checksum: {name}")
		result[f"plugins/{name}"] = value
	return result


def export_closure(roots, destination):
	paths = set()
	for root in roots:
		for value in run("nix-store", "--query", "--requisites", str(root)).splitlines():
			path = Path(value)
			if (path.parent != Path("/nix/store") or not STORE_NAME.fullmatch(path.name)
				or not os.path.lexists(path)):
				raise ValueError("closure query returned a missing or non-root store path")
			paths.add(value)
	if not paths or not set(map(str, roots)).issubset(paths):
		raise ValueError("incomplete runtime closure")
	with destination.open("xb") as stream:
		subprocess.run(["nix-store", "--export", *sorted(paths)], stdout=stream, check=True)


def verify_archive(archive, root, expected):
	"""Verify every archived payload without extraction."""
	seen = set()
	directories = {root, f"{root}/bin", f"{root}/plugins"}
	with tarfile.open(archive, mode="r|gz") as tar:
		for member in tar:
			if member.name in seen:
				raise ValueError("duplicate archive member")
			seen.add(member.name)
			if member.isdir() and member.name in directories:
				continue
			prefix = root + "/"
			name = member.name.removeprefix(prefix)
			if not member.isfile() or not member.name.startswith(prefix) or name not in expected:
				raise ValueError("unexpected archive member")
			with tar.extractfile(member) as stream:
				if hashlib.file_digest(stream, "sha256").hexdigest() != expected[name]:
					raise ValueError(f"archive payload checksum mismatch: {name}")
	if seen != directories | {f"{root}/{name}" for name in expected}:
		raise ValueError("archive has missing members")


def main():
	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument("--source", type=Path, required=True)
	parser.add_argument("--artifacts", type=Path, required=True)
	parser.add_argument("--cln-store", type=Path, required=True)
	parser.add_argument("--grpc-store", type=Path, required=True)
	parser.add_argument("--core-store", type=Path, required=True)
	parser.add_argument("--hold-lib-dir", type=Path, action="append", required=True)
	parser.add_argument("--output", type=Path, required=True)
	args = parser.parse_args()
	output = args.output.absolute()
	if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", output.name):
		raise ValueError("output name must contain only letters, digits, dots, underscores or hyphens")
	archive = output.with_name(output.name + ".tar.gz")
	sidecar = archive.with_name(archive.name + ".sha256")
	if any(os.path.lexists(path) for path in (output, archive, sidecar)):
		raise ValueError("output directory, archive and checksum must not already exist")
	if not output.parent.is_dir():
		raise ValueError("output parent directory must already exist")
	source = args.source.resolve(strict=True)
	artifacts = args.artifacts.resolve(strict=True)
	commit = run("git", "-C", str(source), "rev-parse", "HEAD")
	if not re.fullmatch(r"[0-9a-f]{40}", commit):
		raise ValueError("source must have a full Git commit ID")
	if run("git", "-C", str(source), "status", "--porcelain", "--untracked-files=all"):
		raise ValueError("source must have no tracked or untracked changes")
	epoch = int(run("git", "-C", str(source), "show", "-s", "--format=%ct", commit))
	checksums = checksum_file(regular_file(artifacts, "SHA256SUMS.uncompressed"))
	qualification = json.loads(regular_file(artifacts, "release-artifacts.json").read_text(),
		object_pairs_hook=unique_json_object)
	if qualification["source_commit"] != commit:
		raise ValueError("artifact source commit differs from the clean source")
	for name, value in plugin_checksums(qualification).items():
		if name in checksums and checksums[name] != value:
			raise ValueError("plugin checksum records disagree")
		checksums[name] = value
	files = [f"bin/{name}" for name in PROGRAMS] + [f"plugins/{name}" for name in PLUGINS]
	for name in files:
		if digest(regular_file(artifacts, name)) != checksums[name]:
			raise ValueError(f"artifact checksum mismatch: {name}")
	for name in PROGRAMS:
		version = run(str(artifacts / "bin" / name), "--version")
		if not re.search(r"(?:\+|\()" + commit + r"\)?$", version):
			raise ValueError(f"program does not report the source commit: {name}")

	cln, grpc, core = [store_root(p) for p in (args.cln_store, args.grpc_store, args.core_store)]
	if any(p.resolve() != root for p, root in zip(
		(args.cln_store, args.grpc_store, args.core_store), (cln, grpc, core)
	)):
		raise ValueError("node and plugin arguments must name whole store outputs")
	flake = f"git+{source.as_uri()}?rev={commit}#clightning.outPath"
	expected_cln = run("nix", "--extra-experimental-features", "nix-command flakes", "eval",
		"--offline", "--no-write-lock-file", "--raw", flake)
	if str(cln) != expected_cln:
		raise ValueError("CLN store output differs from the committed source pin")
	if run(str(cln / "bin/lightningd"), "--version") != "v26.06.6-abandon6":
		raise ValueError("unexpected Core Lightning version")
	grpc_files = {p.resolve() for p in grpc.rglob("cln-grpc") if p.is_file()
		and digest(p) == checksums["plugins/cln-grpc"]}
	if len(grpc_files) != 1:
		raise ValueError("grpc store output does not contain one qualified plugin")
	store_root(grpc_files.pop())
	if not (core / "bin/bitcoin-cli").is_file() or not (core / "bin/bitcoind").is_file():
		raise ValueError("Core output lacks bitcoin-cli or bitcoind")
	if not re.match(r"Bitcoin Core version v31\.0(?:\.0)?(?:\s|$)",
		run(str(core / "bin/bitcoind"), "--version")):
		raise ValueError("unexpected Bitcoin Core version")
	libraries = sorted(set(p.resolve(strict=True) for p in args.hold_lib_dir))
	for path in libraries:
		store_root(path)
		if not path.is_dir() or not re.fullmatch(r"/nix/store/[A-Za-z0-9+._?=/\-]+", str(path)):
			raise ValueError("invalid hold library directory")
	for name in ("libsqlite3.so", "libpq.so", "libssl.so", "libcrypto.so", "libstdc++.so.6", "libgcc_s.so.1"):
		if not any((path / name).is_file() for path in libraries):
			raise ValueError(f"hold runtime lacks {name}")

	output.mkdir(mode=0o755)
	for name in files:
		target = output / name
		target.parent.mkdir(exist_ok=True, mode=0o755)
		shutil.copyfile(regular_file(artifacts, name), target)
		target.chmod(0o755)
		if digest(target) != checksums[name]:
			raise ValueError(f"artifact changed during copy: {name}")
	for name, roots in (
		("cln-abandon6", [cln]), ("cln-grpc", [grpc]), ("bitcoind", [core]),
		("hold-runtime", sorted(set(store_root(path) for path in libraries))),
	):
		export_closure(roots, output / f"{name}-closure.nar")
	(output / "cln-runtime.env").write_text(
		"LD_LIBRARY_PATH=" + ":".join(map(str, libraries)) + "\n"
		+ f"PATH={core}/bin:/usr/bin:/bin\n"
	)
	for original, name in DOCUMENTS.items():
		content = subprocess.check_output(["git", "-C", str(source), "show", f"{commit}:{original}"])
		text = content.decode("utf-8")
		for original_link, bundle_link in DOCUMENTS.items():
			text = text.replace(f"]({original_link})", f"]({bundle_link})")
			text = text.replace(f"](../{original_link})", f"]({bundle_link})")
		(output / name).write_text(text)
	(output / "INSTALL.md").write_text(
		"# Runtime installation\n\n"
		"Verify the archive checksum, then SHA256SUMS before installation. Import "
		"every *-closure.nar with nix-store --import. Do not build on the release "
		"host. Point the release's runtime-cln symlink to the CLN store output in "
		"release-artifacts.json. Load cln-runtime.env only in the CLN service. "
		"Configure CLN's bitcoin-cli to the Core store output's bin/bitcoin-cli. "
		"Configure both bundled plugins as important plugins.\n\n"
		"Use fresh application state and PostgreSQL 17. Validate both configs "
		"before start. Keep admin ports on loopback, preserve identities and "
		"follow OPERATOR.md and ROLLBACK.md. The bundle contains no deployment "
		"configs, wallets, seeds, credentials or private backups. Bark and barkd "
		"are test helpers in this server candidate, not a client release.\n"
	)
	metadata = {"schema_version": 1, "source_commit": commit,
		"programs": {name: checksums[f"bin/{name}"] for name in PROGRAMS},
		"plugins": {name: checksums[f"plugins/{name}"] for name in PLUGINS},
		"runtime_store_paths": {"cln": str(cln), "grpc": str(grpc), "core": str(core)},
		"hold_library_dirs": list(map(str, libraries)),
		"lightningd_sha256": digest(cln / "bin/lightningd")}
	(output / "release-artifacts.json").write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n")
	paths = sorted(path for path in output.rglob("*") if path.is_file())
	for path in paths:
		if path.relative_to(output).parts[0] not in ("bin", "plugins"):
			path.chmod(0o644)
	expected = {path.relative_to(output).as_posix(): digest(path) for path in paths}
	(output / "SHA256SUMS").write_text("".join(
		f"{value}  {name}\n" for name, value in expected.items()
	))
	(output / "SHA256SUMS").chmod(0o644)
	expected["SHA256SUMS"] = digest(output / "SHA256SUMS")

	def normalized(info):
		info.uid = info.gid = 0
		info.uname = info.gname = ""
		info.mtime = epoch
		info.mode = 0o755 if info.isdir() or info.name.startswith((output.name + "/bin/", output.name + "/plugins/")) else 0o644
		info.pax_headers = {}
		return info

	with archive.open("xb") as stream:
		with gzip.GzipFile(filename="", mode="wb", fileobj=stream, mtime=epoch, compresslevel=1) as compressed:
			with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.PAX_FORMAT) as tar:
				for name in ("", "bin", "plugins", *sorted(expected)):
					path = output / name
					if path.is_symlink():
						raise ValueError("bundle member became a symlink")
					tar.add(path, arcname=output.name + ("/" + name if name else ""), recursive=False, filter=normalized)
	verify_archive(archive, output.name, expected)
	archive_hash = digest(archive)
	with sidecar.open("x") as stream:
		stream.write(archive_hash + "  " + archive.name + "\n")
	print(json.dumps({"archive": str(archive), "sha256": archive_hash, "source_commit": commit}))


if __name__ == "__main__":
	main()
