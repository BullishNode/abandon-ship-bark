{ pkgs, rustBuildToolchain }:

let
	rustPlatform = pkgs.makeRustPlatform {
		cargo = rustBuildToolchain;
		rustc = rustBuildToolchain;
	};
in rustPlatform.buildRustPackage rec {
	pname = "cln-grpc";
	version = "26.06.6";
	src = pkgs.fetchFromGitHub {
		owner = "ElementsProject";
		repo = "lightning";
		rev = "v${version}";
		hash = "sha256-bra45wREkyt4byY7/oemRQsmqSiVX/8vVuwYcYjcQHQ=";
	};
	buildAndTestSubdir = "plugins/grpc-plugin";
	patches = [ ../contrib/cln-grpc/structured-xpay-errors.patch ];
	nativeBuildInputs = [ rustBuildToolchain pkgs.protobuf ];
	buildInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
		pkgs.darwin.apple_sdk.frameworks.Security
	];
	doCheck = false;
	cargoLock.lockFile = "${src}/Cargo.lock";
	cargoHash = "sha256-UOhoqVs7nxZ98v2lJrAOc/qT8bcSPHekloUObI7wuJc=";
	postUnpack = ''
		rm ${src.name}/configure
	'';
}
