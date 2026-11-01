#!/usr/bin/env python3
"""Build the private developer APK for the native Minecraft 1.16.5 target.

The resulting package is ``dev.horizon.android1165`` (label ``Horizon 1.16.5
Dev``).  It contains exactly one native library, ``libhorizon_1_16_5.so``, and
the prepared private inputs bundled as ``assets/1165.zip`` (``world/...``,
``tables.json`` and ``client.jar`` at the zip root).

The inputs stay in ``.local/`` and never enter Git.  The Rust library is built
with the workspace's release profile (opt-level 3, thin LTO); APK staging, Gradle's build directory and
the finished APK all stay under ``.local/android-1165``.  Every APK is signed
with the repository's committed developer key
(``packaging/android-1165/keystore/horizon-1165-dev.jks``), so consecutive
builds share one certificate and install over each other.

Usage:
    python3 tools/android_build_1165.py --abi arm64-v8a
    python3 tools/android_build_1165.py --abi arm64-v8a --skip-rust
    python3 tools/android_build_1165.py --abi arm64-v8a \
        --sdk /opt/android-sdk --java-home /usr/lib/jvm/java-21-openjdk-amd64 \
        --gradle /opt/gradle/gradle-8.14.3/bin/gradle

Signing always uses the committed developer keystore
``packaging/android-1165/keystore/horizon-1165-dev.jks`` (alias ``horizon1165``,
password ``horizon1165``); ``--keystore PATH`` and ``--keystore-password VALUE``
exist only for local experiments.  The script never generates a key: a missing
keystore is an error, because a fresh key would break APK updates.

The script is non-interactive.  It needs the Android SDK (build-tools and an
NDK with the ``aarch64-linux-android28-clang`` driver), Java (for apksigner),
Gradle 8.13-8.x, and a Rust toolchain with the matching Android target.  The
APK is written to ``.local/android-1165/apk/<abi>/horizon-1165-dev.apk`` and
verified with ``apksigner`` before the script exits.
"""
import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import zipfile

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT / 'packaging/android-1165'
LOCAL = ROOT / '.local/android-1165'
VERSION_DIR = ROOT / '.local/versions/1.16.5'
LIBRARY_NAME = 'libhorizon_1_16_5.so'
ASSET_NAME = '1165.zip'
TARGETS = {
    'arm64-v8a': 'aarch64-linux-android',
    'x86_64': 'x86_64-linux-android',
    'armeabi-v7a': 'armv7-linux-androideabi',
    'x86': 'i686-linux-android',
}
MIN_SDK = 28

# The developer signing key is committed on purpose: every APK must carry the
# same certificate so a new build installs over an older one.  It signs only
# this private dev APK and must never be used for a public release.
DEV_KEYSTORE = PROJECT / 'keystore' / 'horizon-1165-dev.jks'
DEV_KEY_ALIAS = 'horizon1165'
DEV_KEYSTORE_PASSWORD = 'horizon1165'
# Gradle reads these so the password never appears in the logged command line.
DEV_KEYSTORE_ENV = 'HORIZON_1165_KEYSTORE'
DEV_KEYSTORE_PASSWORD_ENV = 'HORIZON_1165_KEYSTORE_PASSWORD'


class BuildError(RuntimeError):
    """A user-actionable build failure."""


def log(message):
    print(message, flush=True)


def require(condition, message):
    if not condition:
        raise BuildError(message)


def version_key(path):
    match = re.search(r'gradle-(\d+(?:\.\d+)*)', str(path))
    if match:
        return tuple(int(part) for part in match.group(1).split('.'))
    return ()


def latest(directory):
    entries = [path for path in directory.iterdir() if path.is_dir()]
    require(entries, f'no versioned directories under {directory}')
    return max(entries, key=lambda path: tuple(
        int(part) for part in re.findall(r'\d+', path.name)) or (0,))


def run(command, **kwargs):
    command = [str(part) for part in command]
    print('+ ' + ' '.join(command), flush=True)
    try:
        subprocess.run(command, check=True, **kwargs)
    except FileNotFoundError as error:
        raise BuildError(f'executable not found: {command[0]}') from error
    except OSError as error:
        raise BuildError(f'could not execute {command[0]}: {error}') from error
    except subprocess.CalledProcessError as error:
        raise BuildError(
            f'command failed with exit code {error.returncode}: '
            + ' '.join(command)) from error


def default_sdk():
    candidates = []
    for variable in ('ANDROID_HOME', 'ANDROID_SDK_ROOT'):
        value = os.environ.get(variable)
        if value:
            candidates.append(Path(value))
    candidates.extend([Path('/opt/android-sdk'), Path.home() / 'Android/Sdk'])
    for candidate in candidates:
        if candidate.is_dir():
            return candidate
    return candidates[0]


def default_java_home():
    value = os.environ.get('JAVA_HOME')
    if value:
        return Path(value)
    java = shutil.which('java')
    if java:
        return Path(java).resolve().parent.parent
    return Path('/usr/lib/jvm/java-21-openjdk-amd64')


def find_gradle(explicit):
    if explicit is not None:
        gradle = Path(explicit).expanduser()
        require(gradle.is_file(), f'Gradle executable not found: {gradle}')
        return gradle

    suffix = '.bat' if os.name == 'nt' else ''
    candidates = []
    gradle_home = os.environ.get('GRADLE_HOME')
    if gradle_home:
        candidates.append(Path(gradle_home) / 'bin' / f'gradle{suffix}')
    discovered = shutil.which('gradle')
    if discovered:
        candidates.append(Path(discovered))
    candidates.extend(Path('/opt/gradle').glob(f'*/bin/gradle{suffix}'))
    candidates.extend((Path.home() / '.gradle/wrapper/dists').glob(
        f'gradle-*/*/gradle-*/bin/gradle{suffix}'))
    candidates = [path for path in candidates if path.is_file()]
    require(candidates, 'Gradle not found; install Gradle 8.13+ or pass --gradle PATH')
    # AGP 8.11.1 supports Gradle 8.13 through 8.x, not Gradle 9.
    compatible = [path for path in candidates
                  if (8, 13) <= version_key(path) < (9, 0)]
    require(compatible,
            'Gradle 8.13-8.x was not found (AGP 8.11.1); '
            'pass --gradle PATH to a supported Gradle')
    return max(compatible, key=version_key)


def cargo_target_dir(environment):
    configured = environment.get('CARGO_TARGET_DIR')
    if configured:
        path = Path(configured).expanduser()
        return path if path.is_absolute() else (ROOT / path).resolve()
    return ROOT / 'target'


def build_assets_zip(output, world, tables, client_jar):
    """Create the asset zip with world/, tables.json and client.jar at its root."""
    output.parent.mkdir(parents=True, exist_ok=True)
    partial = output.with_name(output.name + '.partial')
    world_files = sorted(
        (path for path in world.rglob('*') if path.is_file()),
        key=lambda path: path.relative_to(world).as_posix())
    with zipfile.ZipFile(partial, 'w', allowZip64=True) as archive:
        for path in world_files:
            archive.write(
                path,
                'world/' + path.relative_to(world).as_posix(),
                compress_type=zipfile.ZIP_DEFLATED,
                compresslevel=1)
        archive.write(
            tables, 'tables.json',
            compress_type=zipfile.ZIP_DEFLATED, compresslevel=1)
        # A jar is already compressed; storing it keeps builds fast.
        archive.write(client_jar, 'client.jar', compress_type=zipfile.ZIP_STORED)
    partial.replace(output)
    log(f'Bundled assets: {output} '
        f'({len(world_files)} world files, {output.stat().st_size / (1024 * 1024):.1f} MiB)')


def resolve_keystore(override):
    """Return the signing keystore, refusing to invent one when it is missing."""
    keystore = (Path(override).expanduser() if override else DEV_KEYSTORE).resolve()
    require(
        keystore.is_file(),
        f'signing keystore not found: {keystore}\n'
        'The developer key is committed at '
        f'{DEV_KEYSTORE.relative_to(ROOT).as_posix()}. Restore it with '
        '"git checkout -- packaging/android-1165/keystore"; never generate a '
        'new key, because a different certificate makes Android refuse to '
        'install the APK over an older build.')
    return keystore


def build(args):
    abi = args.abi
    target = TARGETS[abi]
    sdk = Path(args.sdk).expanduser() if args.sdk else default_sdk()
    java_home = Path(args.java_home).expanduser() if args.java_home else default_java_home()
    gradle = find_gradle(args.gradle)
    suffix = '.exe' if os.name == 'nt' else ''

    require(sdk.is_dir(),
            f'Android SDK directory not found: {sdk} (pass --sdk PATH)')
    build_tools_root = sdk / 'build-tools'
    require(build_tools_root.is_dir(),
            f'Android build-tools not found under {sdk}; install build-tools 35.0.0')
    build_tools = latest(build_tools_root)
    apksigner = build_tools / 'lib' / 'apksigner.jar'
    require(apksigner.is_file(), f'apksigner.jar not found: {apksigner}')
    platform = sdk / 'platforms' / 'android-35'
    require((platform / 'android.jar').is_file(),
            f'Android platform 35 not found under {sdk}; '
            'install it with: sdkmanager "platforms;android-35"')

    ndk_root = sdk / 'ndk'
    require(ndk_root.is_dir(),
            f'Android NDK not found under {sdk}; install NDK 30')
    ndk = latest(ndk_root)
    if os.name == 'nt':
        host_tag = 'windows-x86_64'
    elif sys.platform == 'darwin':
        host_tag = 'darwin-x86_64'
    else:
        host_tag = 'linux-x86_64'
    ndk_bin = ndk / 'toolchains/llvm/prebuilt' / host_tag / 'bin'
    compiler_target = ('armv7a-linux-androideabi' if abi == 'armeabi-v7a'
                       else target)
    clang = ndk_bin / f'{compiler_target}{MIN_SDK}-clang{suffix}'
    cxx = ndk_bin / f'{compiler_target}{MIN_SDK}-clang++{suffix}'
    ar = ndk_bin / f'llvm-ar{suffix}'
    strip = ndk_bin / f'llvm-strip{suffix}'
    require(clang.is_file(), f'NDK compiler not found: {clang}')
    require(cxx.is_file(), f'NDK C++ compiler not found: {cxx}')
    require(ar.is_file(), f'NDK archiver not found: {ar}')
    require(strip.is_file(), f'NDK strip tool not found: {strip}')

    java = java_home / 'bin' / f'java{suffix}'
    require(java.is_file(), f'Java executable not found: {java}; pass --java-home PATH')

    world = VERSION_DIR / 'world'
    tables = VERSION_DIR / 'tables.json'
    client_jar = VERSION_DIR / 'inputs/client.jar'
    if not client_jar.is_file():
        client_jar = VERSION_DIR / 'client.jar'
    require(world.is_dir(), f'prepared world directory not found: {world}')
    require(tables.is_file(), f'prepared tables not found: {tables}')
    require(client_jar.is_file(),
            f'prepared client.jar not found under {VERSION_DIR} '
            '(expected inputs/client.jar)')
    license_file = ROOT / 'LICENSE'
    notices = ROOT / 'THIRD_PARTY_NOTICES.md'
    require(license_file.is_file(), f'license file not found: {license_file}')
    require(notices.is_file(), f'third-party notices not found: {notices}')

    cargo = None
    if not args.skip_rust:
        cargo = shutil.which('cargo')
        require(cargo, 'cargo was not found on PATH; install Rust or pass --skip-rust')

    log(f'Building {abi} developer APK')
    log(f'  SDK:    {sdk}')
    log(f'  NDK:    {ndk}')
    log(f'  Java:   {java_home}')
    log(f'  Gradle: {gradle}')

    environment = dict(os.environ)
    environment['PATH'] = os.pathsep.join(
        [str(ndk_bin), str(java_home / 'bin'), environment.get('PATH', '')])
    environment['JAVA_HOME'] = str(java_home)
    environment['ANDROID_HOME'] = str(sdk)
    environment['ANDROID_SDK_ROOT'] = str(sdk)
    triple = target.replace('-', '_')
    environment[f'CC_{triple}'] = str(clang)
    environment[f'CXX_{triple}'] = str(cxx)
    environment[f'AR_{triple}'] = str(ar)
    environment[f'CARGO_TARGET_{triple.upper()}_LINKER'] = str(clang)
    target_dir = cargo_target_dir(environment)
    environment['CARGO_TARGET_DIR'] = str(target_dir)

    if cargo is not None:
        run([cargo, 'build', '-p', 'horizon-1-16-5', '--lib',
             '--target', target, '--release'], cwd=ROOT, env=environment)

    library = target_dir / target / 'release' / LIBRARY_NAME
    staging = LOCAL / 'gradle-jni' / abi
    staging.mkdir(parents=True, exist_ok=True)
    staged_library = staging / LIBRARY_NAME
    if library.is_file():
        for stale in staging.glob('*.so'):
            stale.unlink()
        shutil.copyfile(library, staged_library)
    else:
        require(args.skip_rust and staged_library.is_file(),
                f'Rust library not found: {library} '
                '(build it with cargo or omit --skip-rust)')
        log(f'Reusing staged library: {staged_library}')
    run([strip, '--strip-unneeded', staged_library])
    log(f'Staged native library: {staged_library}')

    assets = LOCAL / 'gradle-assets'
    assets.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(license_file, assets / 'LICENSE.txt')
    shutil.copyfile(notices, assets / 'THIRD_PARTY_NOTICES.md')
    build_assets_zip(assets / ASSET_NAME, world, tables, client_jar)

    keystore = resolve_keystore(args.keystore)
    environment[DEV_KEYSTORE_ENV] = str(keystore)
    environment[DEV_KEYSTORE_PASSWORD_ENV] = args.keystore_password
    log(f'Signing keystore: {keystore} (alias {DEV_KEY_ALIAS})')

    run([gradle, '--no-daemon', '-p', PROJECT, f'-Pabi={abi}', 'assembleDebug'],
        cwd=ROOT, env=environment)
    apk_dir = LOCAL / 'gradle-build' / 'outputs' / 'apk' / 'debug'
    require(apk_dir.is_dir(), f'Gradle did not produce an APK directory: {apk_dir}')
    built = apk_dir / 'Horizon1165-debug.apk'
    if not built.is_file():
        candidates = sorted(apk_dir.glob('*.apk'), key=lambda path: path.stat().st_mtime)
        require(candidates, f'no APK found under {apk_dir}')
        built = candidates[-1]

    output = LOCAL / 'apk' / abi / 'horizon-1165-dev.apk'
    output.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(built, output)

    run([java, '-jar', apksigner, 'verify', '--verbose', output])

    with zipfile.ZipFile(output) as apk:
        names = apk.namelist()
        libraries = [name for name in names
                     if name == LIBRARY_NAME or name.endswith('/' + LIBRARY_NAME)]
        require(len(libraries) == 1,
                f'expected one {LIBRARY_NAME} in the APK, found {libraries}')
        require(f'assets/{ASSET_NAME}' in names,
                f'APK is missing assets/{ASSET_NAME}')

    log(f'APK: {output}')
    return output


def main(argv=None):
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--abi', choices=sorted(TARGETS), default='arm64-v8a',
                        help='Android ABI to build (default: arm64-v8a)')
    parser.add_argument('--sdk', type=Path,
                        help='Android SDK root (default: ANDROID_HOME or /opt/android-sdk)')
    parser.add_argument('--java-home', type=Path,
                        help='JDK root containing bin/java')
    parser.add_argument('--gradle', type=Path,
                        help='Gradle 8.13-8.x executable (otherwise discover it)')
    parser.add_argument('--skip-rust', action='store_true',
                        help='Reuse the existing cargo release library')
    parser.add_argument('--keystore', type=Path,
                        help='Signing keystore override (default: the committed '
                             'packaging/android-1165/keystore/horizon-1165-dev.jks)')
    parser.add_argument('--keystore-password',
                        default=os.environ.get(DEV_KEYSTORE_PASSWORD_ENV,
                                              DEV_KEYSTORE_PASSWORD),
                        help='Keystore and key password (default: the '
                             f'{DEV_KEYSTORE_PASSWORD_ENV} environment variable '
                             f'or {DEV_KEYSTORE_PASSWORD}); never put it on the '
                             'Gradle command line')
    args = parser.parse_args(argv)
    try:
        build(args)
    except BuildError as error:
        print(f'error: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
