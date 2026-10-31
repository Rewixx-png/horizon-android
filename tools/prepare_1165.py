#!/usr/bin/env python3
"""Prepare the pinned Minecraft 1.16.5 tables for the second engine target (M0).

Downloads only what the M0 acceptance needs, verifies every pinned SHA-1, runs
Mojang's own 1.16.5 data generator and writes a checked private bundle:

    .local/versions/1.16.5/
        inputs/version.json, client.jar, server.jar, client_mappings.txt,
                server_mappings.txt, assets/indexes/1.16.json
        reports/blocks.json, registries.json, commands.json, biomes/
        tables.json
        summary.json

The generator's `--reports` output is not downloaded and no Minecraft file is
published: inputs and tables stay under `.local/`. `--assets` additionally
downloads the 1.16 asset objects (about 334 MB). Pass `--verify-only` to check
an existing bundle without the network or Java.

M0 only produces data. It does not add a runtime, a downloader, or a launcher
version button; those start at M1. The table schema is documented in
`docs/versions/1.16.5.md`.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
LOCK = ROOT / "docs" / "versions" / "1.16.5.lock.json"
DEFAULT_STORE = ROOT / ".local" / "versions" / "1.16.5"
USER_AGENT = "horizon-1.16.5-prepare/1"
REPORT_FILES = ("blocks.json", "registries.json", "commands.json")


class PrepareError(RuntimeError):
    pass


def sha1(path):
    digest = hashlib.sha1()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_json(path):
    with path.open(encoding="utf-8") as stream:
        return json.load(stream)


def write_json(path, value, *, pretty=False):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.partial")
    with temporary.open("w", encoding="utf-8") as stream:
        if pretty:
            json.dump(value, stream, indent=2, sort_keys=True)
            stream.write("\n")
        else:
            json.dump(value, stream, separators=(",", ":"), sort_keys=True)
    temporary.replace(path)


def download(item, path):
    """Download one locked file atomically and verify both byte count and SHA-1."""
    expected, size = item["sha1"], item.get("size")
    if path.is_file() and path.stat().st_size == size and sha1(path) == expected:
        return path
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.partial")
    request = urllib.request.Request(item["url"], headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=120) as response, temporary.open("wb") as stream:
            shutil.copyfileobj(response, stream, 1024 * 1024)
        actual_size = temporary.stat().st_size
        if actual_size != size:
            raise PrepareError(f"{path.name}: expected {size} bytes, got {actual_size}")
        actual = sha1(temporary)
        if actual != expected:
            raise PrepareError(f"{path.name}: expected SHA-1 {expected}, got {actual}")
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)
    return path


def asset_objects(index):
    """[(url, relative object path, sha1, size)] from a 1.16 asset index, deduplicated."""
    found = {}
    for name, value in index.get("objects", {}).items():
        checksum, size = value.get("hash"), value.get("size")
        if not isinstance(checksum, str) or not re.fullmatch(r"[0-9a-f]{40}", checksum):
            raise PrepareError(f"asset {name!r} has an unsafe hash")
        if not isinstance(size, int) or size < 0:
            raise PrepareError(f"asset {name!r} has an unsafe size")
        relative = f"{checksum[:2]}/{checksum}"
        found[relative] = (f"https://resources.download.minecraft.net/{relative}", relative, checksum, size)
    return list(found.values())


def inputs(lock, store, assets=False, jobs=8):
    """Download and verify the five pinned inputs, and optionally every asset object."""
    folder = store / "inputs"
    targets = {
        "version": (folder / "version.json", lock["version_json"]),
        "client": (folder / "client.jar", lock["client"]),
        "server": (folder / "server.jar", lock["server"]),
        "client_mappings": (folder / "client_mappings.txt", lock["client_mappings"]),
        "server_mappings": (folder / "server_mappings.txt", lock["server_mappings"]),
        "asset_index": (folder / "assets" / "indexes" / f"{lock['asset_index']['id']}.json", lock["asset_index"]),
    }
    for name, (path, item) in targets.items():
        print(f"Verifying {name}: {path.name}", flush=True)
        download(item, path)
    index = read_json(targets["asset_index"][0])
    if assets:
        objects = asset_objects(index)
        print(f"Verifying {len(objects)} asset objects", flush=True)

        def one(entry):
            url, relative, checksum, size = entry
            download({"url": url, "sha1": checksum, "size": size}, folder / "assets" / "objects" / relative)

        with ThreadPoolExecutor(max_workers=jobs) as pool:
            list(pool.map(one, objects))
    else:
        print(f"Asset index lists {len(asset_objects(index))} objects (use --assets to download them)", flush=True)
    return {
        "version": targets["version"][0],
        "client": targets["client"][0],
        "server": targets["server"][0],
        "client_mappings": targets["client_mappings"][0],
        "server_mappings": targets["server_mappings"][0],
        "asset_index": targets["asset_index"][0],
        "assets": assets,
    }


def java_probe(java):
    result = subprocess.run([str(java), "-version"], capture_output=True, text=True)
    text = result.stderr + result.stdout
    found = re.search(r'version "(\d+)(?:\.(\d+))?', text)
    if not found:
        raise PrepareError(f"Could not read the Java version from {java}:\n{text[-1000:]}")
    major = int(found[1])
    if major == 1 and found[2]:
        major = int(found[2])
    return major


def find_java(explicit):
    java = Path(explicit).expanduser() if explicit else (
        Path(os.environ["JAVA_HOME"]) / "bin" / "java" if os.environ.get("JAVA_HOME") else None
    )
    if java is None:
        from shutil import which

        found = which("java")
        if not found:
            raise PrepareError("Java not found: set JAVA_HOME, pass --java, or put java on PATH")
        java = Path(found)
    if not java.is_file():
        raise PrepareError(f"Java not found at {java}")
    major = java_probe(java)
    if major < 8:
        raise PrepareError(f"1.16.5 needs Java 8 or newer; {java} is Java {major}")
    return java, major


def run_datagen(java, server, reports, main):
    """Run Mojang's 1.16.5 `--reports` generator and replace `reports` atomically."""
    store = reports.parent
    scratch = Path(tempfile.mkdtemp(prefix=".datagen-", dir=store))
    out = scratch / "out"
    log = scratch / "datagen.log"
    try:
        print(f"Running 1.16.5 {main} --reports", flush=True)
        with log.open("w", encoding="utf-8") as stream:
            result = subprocess.run(
                [str(java), "-cp", str(server), main, "--reports", "--output", str(out)],
                cwd=scratch,
                stdout=stream,
                stderr=subprocess.STDOUT,
            )
        if result.returncode:
            raise PrepareError(f"The 1.16.5 data generator failed:\n{log.read_text(errors='replace')[-4000:]}")
        produced = out / "reports"
        missing = [name for name in REPORT_FILES if not (produced / name).is_file()]
        if missing:
            raise PrepareError(f"The 1.16.5 data generator did not write {', '.join(missing)}")
        if not (produced / "biomes").is_dir():
            raise PrepareError("The 1.16.5 data generator did not write the biomes report")
        previous = reports.with_name(f".{reports.name}.{os.getpid()}.previous")
        if reports.exists():
            reports.replace(previous)
        produced.replace(reports)
        shutil.rmtree(previous, ignore_errors=True)
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def facts(lock, client_jar):
    with zipfile.ZipFile(client_jar) as archive:
        version = json.loads(archive.read("version.json"))
    if version.get("id") != lock["version"]:
        raise PrepareError(f"version.json says {version.get('id')!r}, expected {lock['version']!r}")
    pack = version.get("pack_version")
    if isinstance(pack, dict):
        resource = pack.get("resource_major")
        data = pack.get("data_major")
    else:
        resource = data = pack
    facts = {
        "version": version["id"],
        "protocol_version": version.get("protocol_version"),
        "world_version": version.get("world_version"),
        "resource_pack_format": resource,
        "data_pack_format": data,
        "stable": version.get("stable"),
        "java_major": lock["facts"]["java_major"],
    }
    for key in ("protocol_version", "world_version", "resource_pack_format", "data_pack_format"):
        if facts[key] != lock["facts"][key]:
            raise PrepareError(f"version.json {key}={facts[key]!r}, lock pins {lock['facts'][key]!r}")
    return facts


def check_blocks(blocks):
    """Return (entries, state count) after validating defaults and global state ids."""
    entries = {}
    seen = set()
    states = 0
    for name, block in blocks.items():
        raw_states = block.get("states")
        if not isinstance(raw_states, list) or not raw_states:
            raise PrepareError(f"block {name} has no states")
        default = None
        clean = []
        for state in raw_states:
            state_id = state.get("id")
            if not isinstance(state_id, int) or state_id < 0:
                raise PrepareError(f"block {name} has an unsafe state id {state_id!r}")
            if state_id in seen:
                raise PrepareError(f"block state id {state_id} appears twice (block {name})")
            seen.add(state_id)
            is_default = bool(state.get("default"))
            if is_default:
                if default is not None:
                    raise PrepareError(f"block {name} has more than one default state")
                default = state_id
            clean.append({"id": state_id, "properties": state.get("properties", {}), "default": is_default})
        if default is None:
            raise PrepareError(f"block {name} has no default state")
        entries[name] = {"default": default, "states": clean}
        states += len(clean)
    return entries, states


def check_registries(registries):
    entries = {}
    for registry, value in registries.items():
        raw = value.get("entries")
        if not isinstance(raw, dict):
            raise PrepareError(f"registry {registry} has no entries")
        ids = {}
        clean = {}
        for name, entry in raw.items():
            protocol = entry.get("protocol_id")
            if protocol is not None:
                if not isinstance(protocol, int) or protocol < 0:
                    raise PrepareError(f"{registry}/{name} has an unsafe protocol id {protocol!r}")
                if protocol in ids:
                    raise PrepareError(f"{registry}: protocol id {protocol} is used by {ids[protocol]} and {name}")
                ids[protocol] = name
            clean[name] = {"protocol_id": protocol} if protocol is not None else {}
        entries[registry] = {
            "default": value.get("default"),
            "count": len(clean),
            "entries": clean,
        }
    return entries


def build_tables(lock, locked_facts, reports, source):
    blocks_raw = read_json(reports / "blocks.json")
    registries_raw = read_json(reports / "registries.json")
    commands = read_json(reports / "commands.json")
    biomes = sorted(path.stem for path in (reports / "biomes").glob("*.json"))
    blocks, block_states = check_blocks(blocks_raw)
    registries = check_registries(registries_raw)
    if "minecraft:block" not in registries or "minecraft:item" not in registries:
        raise PrepareError("registries.json is missing minecraft:block or minecraft:item")
    block_registry = registries["minecraft:block"]["entries"]
    item_registry = registries["minecraft:item"]["entries"]
    if set(block_registry) != set(blocks):
        missing = sorted(set(block_registry) - set(blocks))
        extra = sorted(set(blocks) - set(block_registry))
        raise PrepareError(f"block report and registry differ (missing={missing[:5]}, extra={extra[:5]})")
    tables = {
        "schema": 1,
        "version": lock["version"],
        "facts": locked_facts,
        "source": source,
        "blocks": {
            "count": len(blocks),
            "state_count": block_states,
            "entries": blocks,
        },
        "items": {
            "count": len(item_registry),
            "entries": item_registry,
        },
        "registries": {
            "count": len(registries),
            "entries": registries,
        },
        "commands": commands,
        "biomes": {"count": len(biomes), "names": biomes},
    }
    expected = lock["expected"]
    actual = {
        "blocks": tables["blocks"]["count"],
        "block_states": tables["blocks"]["state_count"],
        "items": tables["items"]["count"],
        "registries": tables["registries"]["count"],
        "biomes": tables["biomes"]["count"],
    }
    if actual != expected:
        raise PrepareError(f"1.16.5 table counts differ from the lock: expected {expected}, got {actual}")
    return tables


def verify_tables(lock, tables):
    if tables.get("schema") != 1 or tables.get("version") != lock["version"]:
        raise PrepareError("tables.json has the wrong schema or version")
    if tables.get("facts", {}).get("protocol_version") != lock["facts"]["protocol_version"]:
        raise PrepareError("tables.json has the wrong protocol")
    counts = {
        "blocks": tables.get("blocks", {}).get("count"),
        "block_states": tables.get("blocks", {}).get("state_count"),
        "items": tables.get("items", {}).get("count"),
        "registries": tables.get("registries", {}).get("count"),
        "biomes": tables.get("biomes", {}).get("count"),
    }
    if counts != lock["expected"]:
        raise PrepareError(f"tables.json counts differ from the lock: {counts}")
    # Revalidate the parts a future consumer relies on, not only the counts.
    blocks, states = check_blocks({name: value for name, value in tables["blocks"]["entries"].items()})
    registries = check_registries({name: value for name, value in tables["registries"]["entries"].items()})
    if states != tables["blocks"]["state_count"]:
        raise PrepareError("tables.json block state count does not match its entries")
    if len(tables["items"]["entries"]) != tables["items"]["count"]:
        raise PrepareError("tables.json item count does not match its entries")
    if set(registries["minecraft:block"]["entries"]) != set(blocks):
        raise PrepareError("tables.json block table and registry differ")


def prepare(lock, store, java, assets, skip_datagen, verify_only, jobs):
    if verify_only:
        reports = store / "reports"
        tables_path = store / "tables.json"
        if not tables_path.is_file() or not reports.is_dir():
            raise PrepareError(f"No prepared bundle at {store}; run without --verify-only first")
        tables = read_json(tables_path)
        verify_tables(lock, tables)
        print(f"Verified {tables_path} ({store})")
        return tables
    found = inputs(lock, store, assets=assets, jobs=jobs)
    reports = store / "reports"
    if not skip_datagen:
        try:
            run_datagen(java, found["server"], reports, lock["datagen"]["main"])
        except PrepareError as error:
            if not (reports.is_dir() and all((reports / name).is_file() for name in REPORT_FILES)):
                raise
            print(f"Reusing the existing reports: {error}", flush=True)
    if not reports.is_dir():
        raise PrepareError("No reports; run without --skip-datagen")
    locked_facts = facts(lock, found["client"])
    source = {
        "version_json_sha1": sha1(found["version"]),
        "client_sha1": sha1(found["client"]),
        "server_sha1": sha1(found["server"]),
        "client_mappings_sha1": sha1(found["client_mappings"]),
        "server_mappings_sha1": sha1(found["server_mappings"]),
        "asset_index_sha1": sha1(found["asset_index"]),
        "assets_downloaded": assets,
    }
    tables = build_tables(lock, locked_facts, reports, source)
    tables_path = store / "tables.json"
    write_json(tables_path, tables)
    summary = {
        "version": lock["version"],
        "protocol": locked_facts["protocol_version"],
        "world_version": locked_facts["world_version"],
        "pack_formats": {
            "resource": locked_facts["resource_pack_format"],
            "data": locked_facts["data_pack_format"],
        },
        "counts": {
            "blocks": tables["blocks"]["count"],
            "block_states": tables["blocks"]["state_count"],
            "items": tables["items"]["count"],
            "registries": tables["registries"]["count"],
            "biomes": tables["biomes"]["count"],
        },
        "samples": {
            "blocks": [name for name in ("minecraft:air", "minecraft:stone", "minecraft:oak_log")],
            "items": [name for name in ("minecraft:air", "minecraft:stone", "minecraft:diamond_sword")],
        },
        "source": source,
        "tables": str(tables_path.relative_to(store)),
    }
    write_json(store / "summary.json", summary, pretty=True)
    verify_tables(lock, read_json(tables_path))
    print(json.dumps(summary, indent=2, sort_keys=True))
    return tables


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--store", type=Path, default=DEFAULT_STORE, help="private output folder")
    parser.add_argument("--java", help="java executable (default: JAVA_HOME/bin/java or PATH)")
    parser.add_argument("--assets", action="store_true", help="also verify/download the 1.16 asset objects")
    parser.add_argument("--skip-datagen", action="store_true", help="reuse reports already in the store")
    parser.add_argument("--verify-only", action="store_true", help="check the bundle without network or Java")
    parser.add_argument("-j", "--jobs", type=int, default=8, help="parallel asset downloads")
    args = parser.parse_args()
    lock = read_json(LOCK)
    try:
        java = None if args.verify_only else find_java(args.java)[0]
        prepare(lock, args.store.expanduser().resolve(), java, args.assets, args.skip_datagen,
                args.verify_only, args.jobs)
    except (OSError, ValueError, KeyError, PrepareError) as error:
        print(f"prepare_1165: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
