#!/usr/bin/env python3
"""Freeze the public working tree, build offline, and publish an owned Swift package."""
from __future__ import annotations

import argparse
import ctypes
import fcntl
import gzip
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
if sys.version_info < (3, 11):
    sys.exit("Python 3.11+ is required for deterministic core packaging")
import tomllib

UNIFFI = "0.32.2"
OWNER = "danmu-swift-core-package-v1\n"
MARKER = ".danmu-core-package"
TREES = ("src", "crates", "assets", "agent-package", "script", "examples", "tests")
ROOT_FILES = ("Cargo.toml", "Cargo.lock", "LICENSE", "THIRD_PARTY_NOTICES.md", "README.md", "build.rs", "rust-toolchain", "rust-toolchain.toml")
EXCLUDED = {".git", ".build", "target", "dist", "vendor", "node_modules", "__pycache__", ".swiftpm", ".DS_Store", ".local-archive"}
TARGETS = {"arm64": "aarch64-apple-darwin", "x86_64": "x86_64-apple-darwin"}


def fail(message):
    raise RuntimeError(message)


def run(args, *, cwd=None, env=None, capture=False):
    print("+ " + " ".join(map(str, args)), file=sys.stderr, flush=True)
    result = subprocess.run(list(map(str, args)), cwd=cwd, env=env, check=True,
                            stdout=subprocess.PIPE if capture else None, text=True)
    return result.stdout.strip() if capture else None


def sha(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def encoded(value):
    return (json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode()


def dump(path, value):
    path.write_bytes(encoded(value))


def safe_path(path):
    path = Path(os.path.abspath(path))
    for component in (path, *path.parents):
        if component.is_symlink():
            fail(f"Refusing symlink path: {component}")
    return path


def files_under(root, excluded=EXCLUDED):
    for base, dirs, files in os.walk(root, followlinks=False):
        dirs[:] = sorted(d for d in dirs if d not in excluded)
        for name in dirs:
            if (Path(base) / name).is_symlink():
                fail(f"Symlink directory in source: {Path(base) / name}")
        for name in sorted(files):
            if name in excluded or ("__pycache__" in excluded and name.endswith((".pyc", ".pyo"))):
                continue
            path = Path(base) / name
            if path.is_symlink() or not path.is_file():
                fail(f"Non-regular input: {path}")
            yield path


def input_paths(root):
    # This deliberately never enumerates HOME, .git, local configuration, or G.
    for name in ROOT_FILES:
        path = root / name
        if path.exists():
            if path.is_symlink() or not path.is_file():
                fail(f"Non-regular input: {path}")
            yield path
    for name in TREES:
        path = root / name
        if path.is_symlink():
            fail(f"Symlink source tree: {path}")
        if path.is_dir():
            yield from files_under(path)
    # Unknown Cargo configuration must not silently change the frozen build.
    if (root / ".cargo").exists():
        fail("Move repository .cargo configuration into the explicit build contract before packaging")


def manifest(root, destination=None):
    entries = []
    for path in sorted(input_paths(root), key=lambda p: p.relative_to(root).as_posix()):
        rel = path.relative_to(root).as_posix()
        data = path.read_bytes()
        mode = 0o755 if path.stat().st_mode & 0o111 else 0o644
        entries.append({"path": rel, "sha256": hashlib.sha256(data).hexdigest(), "size": len(data), "mode": mode})
        if destination is not None:
            out = destination / rel
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_bytes(data)
            out.chmod(mode)
    return {"schema": 1, "files": entries}, hashlib.sha256(encoded(entries)).hexdigest()


def check_contract(root):
    for name in ("Cargo.toml", "Cargo.lock", "LICENSE", "crates/danmu-ffi/Cargo.toml", "crates/danmu-ffi/uniffi.toml", "crates/danmu-ffi/uniffi-bindgen.toml", "crates/danmu-ffi/swift/DanmuCoreFFI/OBSKeychainPasswordStore.swift"):
        if not (root / name).is_file():
            fail(f"Missing packaging prerequisite: {name}")
    crate = tomllib.loads((root / "crates/danmu-ffi/Cargo.toml").read_text())
    dependency = crate.get("dependencies", {}).get("uniffi", {})
    if not isinstance(dependency, dict) or dependency.get("version") != "=" + UNIFFI or dependency.get("default-features") is not False or "tokio" not in dependency.get("features", []):
        fail("danmu-ffi must pin uniffi =0.32.2, default-features=false, features=[tokio]")
    lock = tomllib.loads((root / "Cargo.lock").read_text())
    versions = {p["version"] for p in lock["package"] if p["name"] == "uniffi"}
    if versions != {UNIFFI} or not any(p["name"] == "danmu-ffi" for p in lock["package"]):
        fail("Cargo.lock must already include danmu-ffi and exactly UniFFI 0.32.2; packaging never updates the original lock")
    config = tomllib.loads((root / "crates/danmu-ffi/uniffi.toml").read_text()).get("bindings", {}).get("swift", {})
    for key, value in {"module_name": "DanmuCoreFFI", "ffi_module_name": "DanmuCoreFFIShim", "ffi_module_filename": "DanmuCoreFFIShim"}.items():
        if config.get(key) != value:
            fail(f"uniffi.toml requires {key} = {value}")
    # A local dependency must be part of the public frozen input set, never G/HOME.
    def check_paths(value, base):
        if isinstance(value, dict):
            for key, child in value.items():
                if key == "path" and isinstance(child, str):
                    candidate = Path(child)
                    if candidate.is_absolute() or not (base / candidate).resolve().is_relative_to(root.resolve()):
                        fail(f"Cargo path escapes public source snapshot: {base}")
                else:
                    check_paths(child, base)
        elif isinstance(value, list):
            for child in value:
                check_paths(child, base)
    for path in input_paths(root):
        if path.name == "Cargo.toml":
            check_paths(tomllib.loads(path.read_text()), path.parent)


def toolchain(source):
    if platform.system() != "Darwin":
        fail("macOS with Xcode command-line tools is required")
    for binary in ("cargo", "rustc", "rustup", "xcrun", "xcodebuild", "lipo", "swift"):
        if shutil.which(binary) is None:
            fail(f"Missing prerequisite: {binary}")
    def version(command):
        return run(command, cwd=source, capture=True)
    return {"rustc": version(["rustc", "--version", "--verbose"]),
            "cargo": version(["cargo", "--version"]),
            "swift": version(["swift", "--version"]),
            "xcode": version(["xcodebuild", "-version"]),
            "sdk": version(["xcrun", "--sdk", "macosx", "--show-sdk-version"]),
            "clang": version(["xcrun", "clang", "--version"]),
            "rustup_toolchain": version(["rustup", "show", "active-toolchain"]).split()[0]}


def arches(args):
    return ["arm64", "x86_64"] if args.arch == "universal" else ["arm64"]


def expected(args, source_hash, revision, tools):
    return {"source_sha256": source_hash, "source_revision": revision, "uniffi_version": UNIFFI,
            "configuration": args.configuration, "architectures": arches(args), "test_support": args.test_support,
            "toolchain": tools, "macos_deployment_target": "14.0"}


def package_files(root):
    # Packages have no symlinks, executable tools, or dynamic libraries.
    return sorted(files_under(root, excluded=set()), key=lambda p: p.relative_to(root).as_posix())


def validate(package, wanted):
    if package.is_symlink() or not package.is_dir() or (package / MARKER).read_text() != OWNER:
        fail("Output is not an owned Danmu core package")
    info = json.loads((package / "core-build.json").read_text())
    for key, value in wanted.items():
        if info.get(key) != value:
            fail(f"Core package metadata mismatch: {key}")
    hashes = info.get("artifact_sha256", {})
    actual = {p.relative_to(package).as_posix(): sha(p) for p in package_files(package)
              if p.relative_to(package).as_posix() not in {"core-build.json", MARKER}}
    if not hashes or actual != hashes:
        fail("Core package file inventory/checksums mismatch")
    required = ["Package.swift", "Sources/DanmuCoreFFI/DanmuCoreFFI.swift", "Sources/DanmuCoreFFI/OBSKeychainPasswordStore.swift",
                "LICENSE", "THIRD_PARTY_NOTICES.md", "third-party-licenses.json", "source-inputs.json", "danmu-core-source.tar.gz.sha256"]
    if any(name not in actual for name in required):
        fail("Incomplete Swift package")
    archive = info.get("source_archive")
    if archive != "danmu-core-source.tar.gz" or actual.get(archive) != info.get("source_archive_sha256"):
        fail("Corresponding source archive mismatch")
    if (package / (archive + ".sha256")).read_text() != info["source_archive_sha256"] + "  " + archive + "\n":
        fail("Source archive checksum sidecar mismatch")
    framework = package / "DanmuCoreFFIShim.xcframework"
    data = plistlib.loads((framework / "Info.plist").read_bytes())
    libraries = data.get("AvailableLibraries", [])
    if len(libraries) != 1:
        fail("Expected one macOS XCFramework library")
    library = libraries[0]
    if library.get("SupportedPlatform") != "macos" or sorted(library.get("SupportedArchitectures", [])) != sorted(wanted["architectures"]):
        fail("XCFramework architecture metadata mismatch")
    identifier = library["LibraryIdentifier"]
    binary_name = library["LibraryPath"]
    if not re.fullmatch(r"macos-[A-Za-z0-9_]+", identifier) or binary_name != "libDanmuCoreFFIShim.a":
        fail("Unexpected XCFramework library layout")
    binary = framework / identifier / binary_name
    if sorted(run(["lipo", "-archs", binary], capture=True).split()) != sorted(wanted["architectures"]):
        fail("Actual static library architectures mismatch")
    if any(p.suffix in {".dylib", ".so"} for p in package_files(package)):
        fail("Dynamic library must not be shipped in the static package")
    return info


def vendor(snapshot):
    before = sha(snapshot / "Cargo.lock")
    text = run(["cargo", "vendor", "--locked", "--versioned-dirs", str(snapshot / "vendor")], cwd=snapshot, capture=True)
    if sha(snapshot / "Cargo.lock") != before:
        fail("cargo vendor changed frozen Cargo.lock")
    config = tomllib.loads(text)
    if set(config) != {"source"}:
        fail("Unexpected cargo vendor configuration")
    lines = []
    for name, values in sorted(config["source"].items()):
        lines.append("[source." + json.dumps(name) + "]")
        for key, value in sorted(values.items()):
            if key == "directory":
                value = "vendor"
            if not isinstance(value, str):
                fail("Unsupported vendor configuration value")
            lines.append(key + " = " + json.dumps(value))
        lines.append("")
    (snapshot / ".cargo").mkdir()
    (snapshot / ".cargo/config.toml").write_text("\n".join(lines))


# Some crates omit the repository-wide license from their published subcrate.
# Keep exact-version provenance explicit; unknown omissions still fail closed.
SUPPLEMENTAL_LICENSES = {
    ("alloc-stdlib", "0.2.4"): ("BSD-3-Clause", "https://github.com/dropbox/rust-alloc-no-stdlib",
        ["assets/licenses/alloc-stdlib-BSD-3-Clause.txt"],
        "https://github.com/dropbox/rust-alloc-no-stdlib/blob/ae42d22078b98549e987d2f03d12df7b984fde47/LICENSE"),
    ("alloc-stdlib", "0.3.0"): ("BSD-3-Clause", "https://github.com/dropbox/rust-alloc-no-stdlib",
        ["assets/licenses/alloc-stdlib-BSD-3-Clause.txt"],
        "https://github.com/dropbox/rust-alloc-no-stdlib/blob/0a81fd6928ea3b33c8cd484aa4575d50ffb98012/LICENSE"),
    ("ratatui-termina", "0.1.0"): ("MIT", "https://github.com/ratatui/ratatui",
        ["vendor/ratatui-0.30.2/LICENSE"],
        "https://github.com/ratatui/ratatui/blob/e665c36cb14752a61cd777fbd06dbef8474f2add/LICENSE"),
    ("vtparse", "0.6.2"): ("MIT", "https://github.com/wez/wezterm",
        ["assets/licenses/wezterm-MIT.txt"],
        "https://github.com/wez/wezterm/blob/edeae72b5fc55c7fa4aa1d08bbefd08c5493f757/LICENSE.md"),
    ("wezterm-input-types", "0.1.0"): ("MIT", "https://github.com/wez/wezterm",
        ["assets/licenses/wezterm-MIT.txt"],
        "https://github.com/wez/wezterm/blob/d5ca5509b95edc31291ba29bd1fcbf3f0b12c92d/LICENSE.md"),
}
for _version, _revision in (("5.3.0", "97b55bed1c2c91dcbf787674849f05337ff80b33"),
                            ("6.0.0", "7e1b0322d31d625f81a5656096330934f9cd835d")):
    SUPPLEMENTAL_LICENSES[("r-efi", _version)] = (
        "MIT OR Apache-2.0 OR LGPL-2.1-or-later", "https://github.com/r-efi/r-efi",
        [f"vendor/r-efi-{_version}/AUTHORS"], f"https://github.com/r-efi/r-efi/blob/{_revision}/AUTHORS")
for _name in ("uniffi", "uniffi_bindgen", "uniffi_core", "uniffi_internal_macros",
              "uniffi_macros", "uniffi_meta", "uniffi_pipeline", "uniffi_udl"):
    SUPPLEMENTAL_LICENSES[(_name, UNIFFI)] = (
        "MPL-2.0", "https://github.com/mozilla/uniffi-rs", ["LICENSE"],
        "https://github.com/mozilla/uniffi-rs/blob/7bc621ee9d0d15371d792612707ba5ed4111f950/LICENSE")
for _name in ("winapi-i686-pc-windows-gnu", "winapi-x86_64-pc-windows-gnu"):
    SUPPLEMENTAL_LICENSES[(_name, "0.4.0")] = (
        "MIT/Apache-2.0", "https://github.com/retep998/winapi-rs",
        ["vendor/winapi-0.3.9/LICENSE-MIT", "vendor/winapi-0.3.9/LICENSE-APACHE"],
        "https://github.com/retep998/winapi-rs/tree/796a8e6c2971dc2ff1bcff166e6671284f9b5b6b")


def license_index(snapshot):
    result = []
    for crate in sorted((snapshot / "vendor").iterdir()):
        if not crate.is_dir() or crate.is_symlink():
            fail("Unexpected vendor entry")
        data = tomllib.loads((crate / "Cargo.toml").read_text())["package"]
        licenses = [p.relative_to(snapshot).as_posix() for p in files_under(crate)
                    if p.name.lower().startswith(("license", "licence", "copying", "notice", "copyright"))]
        explicit = data.get("license-file")
        if explicit:
            path = (crate / explicit).resolve()
            if not path.is_relative_to(crate.resolve()) or not path.is_file():
                fail(f"Missing vendored license-file for {crate.name}")
            rel = path.relative_to(snapshot).as_posix()
            if rel not in licenses:
                licenses.append(rel)
        if not data.get("license") and not explicit:
            fail(f"Missing dependency license metadata: {crate.name}")
        license_source = "vendored crate"
        if not licenses:
            supplement = SUPPLEMENTAL_LICENSES.get((data["name"], data["version"]))
            if supplement is None or (data.get("license"), data.get("repository")) != supplement[:2]:
                fail(f"Vendored dependency has no verified license/notice text: {crate.name}")
            _, _, licenses, license_source = supplement
            if any(not (snapshot / path).is_file() or (snapshot / path).is_symlink() for path in licenses):
                fail(f"Missing verified supplemental license text: {crate.name}")
        result.append({"name": data["name"], "version": data["version"], "license": data.get("license"),
                       "license_files": sorted(licenses), "license_source": license_source,
                       "source_directory": "vendor/" + crate.name})
    return result


def deterministic_archive(source, destination):
    with destination.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0, compresslevel=9) as gz:
            with tarfile.open(fileobj=gz, mode="w", format=tarfile.PAX_FORMAT) as tar:
                for path in sorted(files_under(source, excluded=set()), key=lambda p: p.relative_to(source).as_posix()):
                    name = "danmu-core-source/" + path.relative_to(source).as_posix()
                    info = tar.gettarinfo(str(path), arcname=name)
                    info.uid = info.gid = info.mtime = 0
                    info.uname = info.gname = ""
                    info.mode = 0o755 if path.stat().st_mode & 0o111 else 0o644
                    info.pax_headers = {}
                    with path.open("rb") as stream:
                        tar.addfile(info, stream)


def publish(stage, output):
    if not output.exists():
        os.rename(stage, output)
        return
    if not output.is_dir() or output.is_symlink() or (output / MARKER).read_text() != OWNER:
        fail("Refusing to replace an unowned output directory")
    # macOS RENAME_SWAP keeps a valid complete directory at the destination at all times.
    libc = ctypes.CDLL(None, use_errno=True)
    swap = libc.renamex_np
    swap.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
    swap.restype = ctypes.c_int
    if swap(os.fsencode(stage), os.fsencode(output), 2) != 0:
        raise OSError(ctypes.get_errno(), "Atomic package swap failed")
    shutil.rmtree(stage)


def build(args, source, output, wanted, inputs):
    # Work and package stages are sibling private directories; only our own stages are removed.
    with tempfile.TemporaryDirectory(prefix=".danmu-core-freeze-", dir=output.parent) as temporary:
        work = Path(temporary)
        work.chmod(0o700)
        snapshot = work / "source"
        snapshot.mkdir()
        frozen, frozen_hash = manifest(source, snapshot)
        current, current_hash = manifest(source)
        if frozen != inputs or current != inputs or frozen_hash != wanted["source_sha256"] or current_hash != frozen_hash:
            fail("Working-tree inputs changed while freezing; retry after edits settle")
        check_contract(snapshot)
        vendor(snapshot)
        licenses = license_index(snapshot)
        dump(snapshot / "source-inputs.json", inputs)
        dump(snapshot / "core-build-inputs.json", wanted)
        dump(snapshot / "third-party-licenses.json", licenses)
        isolated_home = work / "cargo-home"
        isolated_home.mkdir()
        # Build scripts receive no account/model/OBS secrets or ambient compiler flags.
        build_home = work / "home"
        build_home.mkdir()
        build_tmp = work / "tmp"
        build_tmp.mkdir()
        env = {key: os.environ[key] for key in ("PATH", "DEVELOPER_DIR") if key in os.environ}
        env.update({"HOME": str(build_home), "TMPDIR": str(build_tmp),
                    "RUSTUP_HOME": os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup")),
                    "RUSTUP_TOOLCHAIN": wanted["toolchain"]["rustup_toolchain"]})
        sdk = run(["xcrun", "--sdk", "macosx", "--show-sdk-path"], capture=True)
        env.update({"CARGO_HOME": str(isolated_home), "CARGO_NET_OFFLINE": "true", "SDKROOT": sdk,
                    "MACOSX_DEPLOYMENT_TARGET": "14.0", "SOURCE_DATE_EPOCH": "0", "TZ": "UTC",
                    "RUSTFLAGS": "--remap-path-prefix=" + str(snapshot) + "=/danmu-core-source"})
        installed = run(["rustup", "target", "list", "--installed", "--toolchain", env["RUSTUP_TOOLCHAIN"]], cwd=snapshot, capture=True).splitlines()
        host_text = wanted["toolchain"]["rustc"]
        host = next((line.removeprefix("host: ") for line in host_text.splitlines() if line.startswith("host: ")), "")
        if host not in TARGETS.values():
            fail("Native macOS Rust toolchain required for binding generation")
        targets = [TARGETS[a] for a in wanted["architectures"]]
        if host not in targets:
            targets.append(host)
        for target in targets:
            if target not in installed:
                fail(f"Missing Rust target: {target}; install explicitly before packaging")
        profile = "release" if args.configuration == "release" else "debug"
        mode = ["--release"] if profile == "release" else []
        core_features = ["--features", "test-support"] if args.test_support else []
        core_target = work / "core-target"
        generator_target = work / "bindgen-target"
        for target in targets:
            run(["cargo", "build", "--frozen", "--offline", "-p", "danmu-ffi", "--lib", "--no-default-features",
                 "--target", target, "--target-dir", core_target, *mode, *core_features], cwd=snapshot, env=env)
        run(["cargo", "build", "--frozen", "--offline", "-p", "danmu-ffi", "--bin", "danmu-bindgen", "--no-default-features",
             "--features", "bindgen", "--target", host, "--target-dir", generator_target, *mode], cwd=snapshot, env=env)
        generated = work / "generated"
        generated.mkdir()
        run([generator_target / host / profile / "danmu-bindgen", "generate", "--library",
             core_target / host / profile / "libdanmu_ffi.dylib", "--language", "swift", "--config",
             snapshot / "crates/danmu-ffi/uniffi-bindgen.toml", "--out-dir", generated, "--no-format"], cwd=snapshot, env=env)
        swift = generated / "DanmuCoreFFI.swift"
        header = generated / "DanmuCoreFFIShim.h"
        modulemap = generated / "DanmuCoreFFIShim.modulemap"
        if not all(p.is_file() for p in (swift, header, modulemap)):
            fail("UniFFI generated unexpected module filenames")
        swift_text = swift.read_text()
        if "DanmuCoreFFIShim" not in swift_text or "checksum" not in swift_text.lower():
            fail("Generated Swift is missing shim import/checksum checks")
        stage = Path(tempfile.mkdtemp(prefix=".danmu-core-package-", dir=output.parent))
        try:
            (stage / MARKER).write_text(OWNER)
            sources = stage / "Sources/DanmuCoreFFI"
            sources.mkdir(parents=True)
            shutil.copyfile(swift, sources / swift.name)
            shutil.copyfile(snapshot / "crates/danmu-ffi/swift/DanmuCoreFFI/OBSKeychainPasswordStore.swift", sources / "OBSKeychainPasswordStore.swift")
            headers = work / "headers"
            headers.mkdir()
            shutil.copyfile(header, headers / header.name)
            shutil.copyfile(modulemap, headers / "module.modulemap")
            library = work / "libDanmuCoreFFIShim.a"
            static_libraries = [core_target / TARGETS[a] / profile / "libdanmu_ffi.a" for a in wanted["architectures"]]
            if len(static_libraries) == 1:
                shutil.copyfile(static_libraries[0], library)
            else:
                run(["lipo", "-create", *static_libraries, "-output", library])
            run(["lipo", "-verify_arch", *wanted["architectures"], library])
            run(["xcodebuild", "-create-xcframework", "-library", library, "-headers", headers,
                 "-output", stage / "DanmuCoreFFIShim.xcframework"])
            (stage / "Package.swift").write_text('''// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "DanmuCoreFFI",
    platforms: [.macOS(.v14)],
    products: [.library(name: "DanmuCoreFFI", targets: ["DanmuCoreFFI"])],
    targets: [
        .binaryTarget(name: "DanmuCoreFFIShim", path: "DanmuCoreFFIShim.xcframework"),
        .target(name: "DanmuCoreFFI", dependencies: ["DanmuCoreFFIShim"],
                linkerSettings: [.linkedFramework("Security"), .linkedFramework("SystemConfiguration"),
                                 .linkedFramework("CoreFoundation"), .linkedLibrary("resolv"), .linkedLibrary("c++")])
    ]
)
''')
            for name in ("LICENSE", "THIRD_PARTY_NOTICES.md", "source-inputs.json", "third-party-licenses.json"):
                shutil.copyfile(snapshot / name, stage / name)
            # Include the corresponding generated Swift/C files unmodified, including checksum code.
            bindings = snapshot / "GeneratedBindings"
            shutil.copytree(sources, bindings / "Sources/DanmuCoreFFI")
            shutil.copytree(headers, bindings / "Headers")
            shutil.copyfile(stage / "Package.swift", bindings / "Package.swift")
            archive = stage / "danmu-core-source.tar.gz"
            deterministic_archive(snapshot, archive)
            archive_hash = sha(archive)
            (stage / (archive.name + ".sha256")).write_text(archive_hash + "  " + archive.name + "\n")
            info = dict(wanted, source_archive=archive.name, source_archive_sha256=archive_hash,
                        source_archive_format="tar.gz", source_archive_root="danmu-core-source",
                        source_inputs_manifest="source-inputs.json", license="LICENSE",
                        third_party_notices="THIRD_PARTY_NOTICES.md", third_party_licenses="third-party-licenses.json")
            info["artifact_sha256"] = {p.relative_to(stage).as_posix(): sha(p) for p in package_files(stage)
                                       if p.relative_to(stage).as_posix() != MARKER}
            dump(stage / "core-build.json", info)
            validate(stage, wanted)
            # Do not label an artifact current if source edits arrived during a long build.
            _, final_hash = manifest(source)
            if final_hash != wanted["source_sha256"]:
                fail("Working-tree inputs changed during build; frozen artifact was not published")
            publish(stage, output)
        finally:
            if stage.exists():
                shutil.rmtree(stage)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("build", "validate"))
    parser.add_argument("--source", required=True)
    parser.add_argument("--configuration", choices=("debug", "release"), required=True)
    parser.add_argument("--arch", choices=("arm64", "universal"), required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--test-support", action="store_true")
    args = parser.parse_args()
    if args.configuration == "release" and args.test_support:
        fail("--test-support is allowed only with debug")
    source = safe_path(args.source)
    output = safe_path(args.output)
    if output == source or source.is_relative_to(output) or any(output == source / name or (source / name).is_relative_to(output) or output.is_relative_to(source / name) for name in TREES):
        fail("Output must not overlap source inputs")
    output.parent.mkdir(parents=True, exist_ok=True)
    for parent in (output.parent, *output.parent.parents):
        if any((parent / ".cargo" / name).exists() for name in ("config", "config.toml")):
            fail(f"Ancestor Cargo configuration would affect frozen builds: {parent}")
    lock_name = ".danmu-core-" + hashlib.sha256(str(output).encode()).hexdigest()[:16] + ".lock"
    fd = os.open(output.parent / lock_name, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        check_contract(source)
        tools = toolchain(source)
        inputs, source_hash = manifest(source)
        revision = run(["git", "-C", source, "rev-parse", "HEAD"], capture=True)
        wanted = expected(args, source_hash, revision, tools)
        if args.action == "validate":
            validate(output, wanted)
        else:
            if output.exists():
                if not output.is_dir() or not (output / MARKER).is_file() or (output / MARKER).read_text() != OWNER:
                    fail("Refusing to overwrite an output not owned by this packager")
                try:
                    validate(output, wanted)
                    print(str(output))
                    return
                except (RuntimeError, OSError, KeyError, ValueError, subprocess.CalledProcessError) as error:
                    print(f"Rebuilding invalid/stale core cache: {error}", file=sys.stderr)
            build(args, source, output, wanted, inputs)
    print(str(output))


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"Core packaging failed: {error}", file=sys.stderr)
        sys.exit(1)
