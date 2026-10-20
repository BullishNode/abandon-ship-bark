{ pkgs }:

let
	upstreamVersion = "26.06.6";
	version = "${upstreamVersion}-abandon6";
in pkgs.clightning.overrideAttrs (old: {
	inherit version;
	src = pkgs.fetchurl {
		url = "https://github.com/ElementsProject/lightning/releases/download/v${upstreamVersion}/clightning-v${upstreamVersion}.zip";
		hash = "sha256-cZEfzDXkqyRuvH1FMcrPK8OBYGnZZ5jcj3pztAMgfO0=";
	};
	patches = (old.patches or []) ++ [ ../contrib/cln/onchain-refund-depth.patch ];
	makeFlags = [ "VERSION=v${version}" ];
	postPatch = (old.postPatch or "") + ''
		chmod +x devtools/blockreplace.py
		patchShebangs devtools/blockreplace.py
	'';
	preInstall = ''
		mkdir -p $out/libexec/c-lightning/plugins/
		touch $out/libexec/c-lightning/plugins/clnrest
	'';
})
