#!/usr/bin/env python3
"""Rebuild the published release from source and its existing public signatures."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import ssl
import subprocess
import sys
import tempfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parent
REPRO = ROOT / 'reproducibility'


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def run(arguments, capture=False):
    return subprocess.run(arguments, cwd=ROOT, check=True, text=True,
                          stdout=subprocess.PIPE if capture else None).stdout


def cargo_command():
    wrapper = ROOT / 'scripts/cargo.sh'
    return [str(wrapper)] if wrapper.is_file() else ['cargo']


def release():
    value = json.loads((REPRO / 'release.json').read_text())
    version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['package']['version']
    if value['version'] != version:
        raise ValueError('The source version differs from the published reference')
    if sha(REPRO / 'publisher.crt') != value['certificate_file_sha256']:
        raise ValueError('The public certificate differs from the release reference')
    for role, artifact in value['artifacts'].items():
        if sha(REPRO / artifact['signature']) != artifact['signature_sha256']:
            raise ValueError('The public signature differs from the reference: ' + role)
    return value


def check_toolchain():
    expected = json.loads((REPRO / 'toolchain.json').read_text())['versions']
    commands = {'cargo': [*cargo_command(), '--version'], 'makensis': ['makensis', '-VERSION'],
                'gcc': ['x86_64-w64-mingw32-gcc', '--version'],
                'windres': ['x86_64-w64-mingw32-windres', '--version'],
                'linker': ['x86_64-w64-mingw32-ld', '--version'],
                'osslsigncode': ['osslsigncode', '--version']}
    actual = {}
    for name, command in commands.items():
        actual[name] = run(command, capture=True).splitlines()[0].strip()
        if actual[name] != expected[name]:
            raise ValueError('Toolchain mismatch for ' + name + ': expected ' + expected[name]
                             + ', found ' + actual[name] + '. See reproducibility/toolchain.json')
    return actual


def attach_signature(binary, role, reference):
    binary = Path(binary)
    expected = reference['artifacts'][role]
    unsigned = sha(binary)
    if unsigned != expected['unsigned_sha256']:
        raise ValueError('Unsigned ' + role + ' differs from the reference: ' + unsigned
                         + '. The existing signature cannot be applied to this build.')
    with tempfile.TemporaryDirectory(prefix='uhc-link-public-signature-', dir=binary.parent) as temporary:
        signed = Path(temporary) / binary.name
        run(['osslsigncode', 'attach-signature', '-CAfile', str(REPRO / 'publisher.crt'),
             '-require-leaf-hash', 'sha256:' + reference['certificate_sha256'],
             '-time', str(reference['verification_time']),
             '-sigin', str(REPRO / expected['signature']), '-in', str(binary), '-out', str(signed)])
        if sha(signed) != expected['sha256']:
            raise ValueError('Signed ' + role + ' differs from the published reference')
        os.replace(signed, binary)


def finalize_uninstaller(binary, output):
    reference = release()
    attach_signature(binary, 'uninstaller', reference)
    shutil.copyfile(binary, output)


def build(output, offline=False, verify_download=False):
    if sys.platform != 'linux':
        raise RuntimeError('Use Debian 13 x86_64 or WSL2 with Debian, see README.md')
    reference = release()
    toolchain = check_toolchain()
    run([*cargo_command(), 'build', '--release', '--locked', '--target', reference['target'],
         *(['--offline'] if offline else [])])
    target = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target'))
    if not target.is_absolute():
        target = ROOT / target
    compiled = target / reference['target'] / 'release/UHC-Link-Raccourcis.exe'
    with tempfile.TemporaryDirectory(prefix='uhc-link-reproduce-') as temporary:
        stage = Path(temporary)
        runtime = stage / reference['artifacts']['runtime']['file']
        runtime.parent.mkdir()
        shutil.copyfile(compiled, runtime)
        attach_signature(runtime, 'runtime', reference)
        uninstaller = stage / 'Uninstall.exe'
        hook = stage / 'finalize-uninstaller'
        hook.write_text('#!/bin/sh\nexec ' + shlex.join([sys.executable, str(Path(__file__).resolve()),
                        '--finalize-uninstaller', str(uninstaller)]) + ' "$@"\n')
        hook.chmod(0o700)
        installer = stage / reference['artifacts']['installer']['file']
        run(['makensis', '-V2', '-DVERSION=' + reference['version'], '-DPAYLOAD=' + str(runtime),
             '-DOUTPUT=' + str(installer), '-DSIGN_UNINSTALLER_COMMAND=' + str(hook),
             str(ROOT / 'installer/windows.nsi')])
        attach_signature(installer, 'installer', reference)
        if verify_download:
            request = urllib.request.Request(reference['download_url'],
                                            headers={'User-Agent': 'UHC-Link reproducible build verification'})
            with urllib.request.urlopen(request, timeout=30) as response:
                downloaded = response.read()
            if hashlib.sha256(downloaded).hexdigest() != sha(installer):
                raise ValueError('The current public download differs from this release')
        metadata = {'version': reference['version'], 'target': reference['target'],
                    'file': installer.name, 'bytes': installer.stat().st_size, 'sha256': sha(installer),
                    'runtime_file': reference['artifacts']['runtime']['file'], 'runtime_bytes': runtime.stat().st_size,
                    'runtime_sha256': sha(runtime), 'unsigned_runtime_sha256': sha(compiled),
                    'uninstaller_file': uninstaller.name, 'uninstaller_bytes': uninstaller.stat().st_size,
                    'uninstaller_sha256': sha(uninstaller), 'signed': True, 'uninstaller_signed': True,
                    'installer': 'NSIS', 'reproduced_published_release': True,
                    'signature': {'type': 'self-signed', 'certificate_sha256': reference['certificate_sha256'],
                                  'existing_public_signatures': True, 'private_key_used': False,
                                  'publicly_trusted_publisher': False, 'timestamped': False},
                    'verification_time_utc': reference['verification_time_utc'], 'toolchain': toolchain,
                    'public_download_verified': verify_download}
        certificate = ssl.PEM_cert_to_DER_cert((REPRO / 'publisher.crt').read_text())
        if hashlib.sha256(certificate).hexdigest() != reference['certificate_sha256']:
            raise ValueError('The public certificate fingerprint differs from the reference')
        for role, artifact in reference['artifacts'].items():
            if sha(stage / artifact['file']) != artifact['sha256']:
                raise ValueError('Final release artifact mismatch: ' + role)
        output = output.resolve()
        output.mkdir(parents=True, exist_ok=True)
        for artifact in reference['artifacts'].values():
            destination = output / artifact['file']
            destination.parent.mkdir(parents=True, exist_ok=True)
            with tempfile.NamedTemporaryFile(prefix='.reproduce-', dir=destination.parent, delete=False) as pending:
                pending.write((stage / artifact['file']).read_bytes())
                pending_path = Path(pending.name)
            os.replace(pending_path, destination)
        (output / 'UHC-Link-Raccourcis.exe.sha256').write_text(sha(installer) + '  UHC-Link-Raccourcis.exe\n')
        (output / 'UHC-Link-Raccourcis-SelfSigned.cer').write_bytes(certificate)
        (output / 'BUILD-INFO.json').write_text(json.dumps(metadata, indent=2) + '\n')
        print(json.dumps({'reproduced': True, 'file': str(output / installer.name),
                          'sha256': metadata['sha256'], 'public_download_verified': verify_download}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--offline', action='store_true')
    parser.add_argument('--output-dir', type=Path, default=ROOT / 'dist')
    parser.add_argument('--verify-download', action='store_true')
    parser.add_argument('--finalize-uninstaller', type=Path, help=argparse.SUPPRESS)
    parser.add_argument('uninstaller', nargs='?', type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.finalize_uninstaller:
        if args.uninstaller is None:
            parser.error('Missing NSIS uninstaller path')
        finalize_uninstaller(args.uninstaller, args.finalize_uninstaller)
    elif args.uninstaller:
        parser.error('Unexpected path')
    else:
        build(args.output_dir, args.offline, args.verify_download)


if __name__ == '__main__':
    main()
