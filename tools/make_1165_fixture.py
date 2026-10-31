#!/usr/bin/env python3
"""Generate the private flat 1.16.5 world used by the developer APK.

The world is produced by the pinned 1.16.5 server jar downloaded by
``tools/prepare_1165.py``: a superflat plains column, the block platform and
the small oak/glass/brick/wool/iron arrangement at the origin. It is only a
test fixture; the output stays under ``.local/versions/1.16.5/world`` (or the
``--out`` path) and never enters Git.

Usage:
    python3 tools/make_1165_fixture.py
    python3 tools/make_1165_fixture.py --out /tmp/horizon-1165-world
"""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
VERSION = ROOT / '.local/versions/1.16.5'

# Keep the fixture identical to the world the desktop/APK tests were verified
# against. The flat generator writes bedrock/stone/dirt/grass_block layers;
# the commands add the surface platform and a few contrast blocks.
PROPERTIES = """\
level-name=world
level-type=flat
generator-settings={"layers":[{"block":"minecraft:bedrock","height":1},{"block":"minecraft:stone","height":2},{"block":"minecraft:dirt","height":2},{"block":"minecraft:grass_block","height":1}],"biome":"minecraft:plains"}
generate-structures=false
spawn-npcs=false
spawn-animals=false
spawn-monsters=false
online-mode=false
server-port=25599
view-distance=2
simulation-distance=2
max-tick-time=-1
sync-chunk-writes=false
motd=horizon-1165-fixture
"""

COMMANDS = [
    'gamerule doMobSpawning false',
    'forceload add -16 -16 16 16',
    'setworldspawn 0 6 0',
    'fill -6 6 -6 6 6 6 minecraft:oak_planks',
    'fill -5 7 -5 -1 9 -1 minecraft:stone_bricks',
    'fill 1 7 1 5 9 5 minecraft:glass',
    'setblock 0 7 0 minecraft:oak_log',
    'setblock 0 8 0 minecraft:oak_log',
    'setblock 0 9 0 minecraft:oak_log',
    'setblock 3 7 -3 minecraft:bricks',
    'setblock -3 7 3 minecraft:white_wool',
    'setblock 2 7 2 minecraft:grass_block',
    'setblock -2 7 -2 minecraft:iron_block',
    'save-all',
    'stop',
]


def fail(message):
    print(f'error: {message}', file=sys.stderr)
    raise SystemExit(1)


def find_java(explicit):
    if explicit:
        return explicit
    java = shutil.which('java')
    if java:
        return java
    fail('java was not found; install a JDK or pass --java PATH')


def run_server(server, java, out):
    with tempfile.TemporaryDirectory(prefix='horizon-1165-world-') as temporary:
        directory = Path(temporary)
        shutil.copyfile(server, directory / 'server.jar')
        (directory / 'eula.txt').write_text('eula=true\n', encoding='utf-8')
        (directory / 'server.properties').write_text(PROPERTIES, encoding='utf-8')

        process = subprocess.Popen(
            [java, '-Xmx1G', '-jar', 'server.jar', 'nogui'],
            cwd=directory,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        ready = threading.Event()
        lines = []

        def drain():
            assert process.stdout is not None
            for line in process.stdout:
                lines.append(line.rstrip())
                if len(lines) > 400:
                    del lines[:200]
                if 'Done (' in line:
                    ready.set()

        reader = threading.Thread(target=drain, daemon=True)
        reader.start()
        deadline = time.monotonic() + 180
        while not ready.wait(0.5):
            if process.poll() is not None:
                reader.join(timeout=1)
                fail('the 1.16.5 server exited before it was ready:\n' + '\n'.join(lines[-30:]))
            if time.monotonic() > deadline:
                process.kill()
                fail('timed out waiting for the 1.16.5 server to start:\n' + '\n'.join(lines[-30:]))

        assert process.stdin is not None
        for command in COMMANDS:
            process.stdin.write(command + '\n')
            process.stdin.flush()
            time.sleep(0.15)
        # The final `stop` is in COMMANDS; wait for the server to flush the
        # region headers before copying the world directory.
        try:
            process.wait(timeout=180)
        except subprocess.TimeoutExpired:
            process.kill()
            fail('the 1.16.5 server did not stop after `stop`:\n' + '\n'.join(lines[-30:]))
        reader.join(timeout=2)
        if process.returncode != 0:
            fail(f'the 1.16.5 server exited with {process.returncode}:\n' + '\n'.join(lines[-30:]))

        world = directory / 'world'
        if not (world / 'region').is_dir():
            fail('the 1.16.5 server produced no world/region directory')
        region_bytes = sum(path.stat().st_size for path in (world / 'region').glob('*.mca'))
        if region_bytes == 0:
            fail('the generated world contains no region data')
        if out.exists():
            shutil.rmtree(out)
        out.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(world, out)
        total = sum(path.stat().st_size for path in out.rglob('*') if path.is_file())
        return len(list((world / 'region').glob('*.mca'))), total


def main(argv=None):
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--server', type=Path,
                        default=VERSION / 'inputs/server.jar',
                        help='pinned 1.16.5 server jar (default: .local/versions/1.16.5/inputs/server.jar)')
    parser.add_argument('--out', type=Path, default=VERSION / 'world',
                        help='output world directory (default: .local/versions/1.16.5/world)')
    parser.add_argument('--java', help='java executable (default: PATH)')
    args = parser.parse_args(argv)

    server = args.server.expanduser().resolve()
    out = args.out.expanduser().resolve()
    if not server.is_file():
        fail(f'1.16.5 server jar not found: {server}; run tools/prepare_1165.py first')
    java = find_java(args.java)

    print(f'Generating a 1.16.5 fixture world with {server.name}', flush=True)
    regions, total = run_server(server, java, out)
    print(f'Fixture world: {out} ({regions} region files, {total / (1024 * 1024):.1f} MiB)', flush=True)
    return 0


if __name__ == '__main__':
    sys.exit(main())
