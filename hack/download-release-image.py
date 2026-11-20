#!/usr/bin/env python3.11
"""Download the official cagent image and materialize a local Shelter artifact."""

import argparse
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
SOURCE = json.loads((HERE / 'release-image.json').read_text())
TOOLS = Path(os.environ.get('CA_TOOLS_DIR', '/usr/libexec/cagent/bin'))
ATTEMPTS = 8
RETRY_HTTP = {401, 408, 429, 500, 502, 503, 504}
INTERRUPTED_CURL = {18, 28, 52, 55, 56, 92}
RETRY_CURL = INTERRUPTED_CURL | {6, 7}


class TransferError(RuntimeError):
    pass


def log(message):
    print(message, file=sys.stderr, flush=True)


def sha256(path):
    with path.open('rb') as stream:
        return 'sha256:' + hashlib.file_digest(stream, 'sha256').hexdigest()


def header(headers, name):
    values = re.findall(rf'^{name}:[ \t]*([^\r\n]+)', headers, re.I | re.M)
    return values[-1] if values else ''


def request(url, *args, token=None):
    command = [
        'curl', '-q', '--silent', '--show-error', '--connect-timeout', '20',
        '--dump-header', '/dev/stderr', '--write-out', '\n%{http_code}', *args,
    ]
    authorization = None
    if token is not None:
        command.extend(['--header', '@-'])
        authorization = f'Authorization: Bearer {token}\n'.encode()
    result = subprocess.run([*command, url], input=authorization, capture_output=True)
    body, _, code = result.stdout.rpartition(b'\n')
    return result.returncode, int(code or 0), result.stderr.decode(errors='replace'), body


def retry_delay(headers, failures):
    retry_after = header(headers, 'retry-after')
    if retry_after.isdigit():
        return int(retry_after)
    if retry_after:
        try:
            return max(0, (parsedate_to_datetime(retry_after) - datetime.now(timezone.utc)).total_seconds())
        except (TypeError, ValueError):
            pass
    return 2 ** min(failures - 1, 5)


def anonymous_token(challenge, repository):
    params = {key.lower(): value for key, value in re.findall(r'([a-z]+)="([^"]+)"', challenge, re.I)}
    if not challenge.lower().startswith('bearer ') or not params.get('realm', '').startswith('https://'):
        raise TransferError('registry did not provide an HTTPS Bearer challenge')
    code, http, _, body = request(
        params['realm'], '--fail', '--max-time', '60', '--retry', '3', '--get',
        '--data-urlencode', f"service={params['service']}",
        '--data-urlencode', f'scope=repository:{repository}:pull',
    )
    if code or http != 200:
        raise TransferError(f'anonymous registry token request failed (HTTP {http}, curl {code})')
    token = json.loads(body)['token']
    if not token or '\r' in token or '\n' in token:
        raise TransferError('registry returned an invalid Bearer token')
    return token


def fetch_manifest(source):
    location, digest = source.rsplit('@', 1)
    host, repository = location.split('/', 1)
    url = f'https://{host}/v2/{repository}/manifests/{digest}'
    # Curl can suppress the 401 headers with --fail. Discover the Bearer
    # challenge here before using --fail to keep errors out of the disk file.
    args = (
        '--max-time', '60', '--location', '--proto-redir', '=https',
        '--header', 'Accept: application/vnd.oci.image.manifest.v1+json',
    )
    code, http, headers, body = request(url, *args)
    challenge, token = '', None
    if http == 401:
        challenge = header(headers, 'www-authenticate')
        token = anonymous_token(challenge, repository)
        code, http, _, body = request(url, *args, token=token)
    if code or http != 200:
        raise TransferError(f'{host} OCI manifest transfer failed (HTTP {http}, curl {code})')
    return json.loads(body), challenge, token


def download_disk(source, part, blob, digest, size, challenge, token):
    if blob.is_file():
        if blob.stat().st_size == size and sha256(blob) == digest:
            return
        blob.unlink()

    host, repository = source.split('@', 1)[0].split('/', 1)
    speed, seconds = ('65536', '30') if host == 'ghcr.io' else ('1', '90')
    url = f'https://{host}/v2/{repository}/blobs/{digest}'
    failures = 0
    while True:
        offset = part.stat().st_size if part.exists() else 0
        if offset >= size and part.is_file():
            if offset != size or sha256(part) != digest:
                part.unlink()
                raise ValueError('downloaded disk does not match the release size and SHA-256')
            part.replace(blob)
            return

        log(f'Downloading disk from {host} at byte {offset:,} of {size:,}')
        code, http, headers, _ = request(
            url, '--fail', '--location', '--proto-redir', '=https', '--continue-at', '-',
            '--speed-limit', speed, '--speed-time', seconds, '--output', str(part),
            token=token,
        )
        if (code == 0 or (code in INTERRUPTED_CURL and http in (200, 206))) and (
            part.is_file() and part.stat().st_size > offset
        ):
            failures = 0
            continue
        failures += 1
        if failures >= ATTEMPTS or (code not in RETRY_CURL and http not in RETRY_HTTP):
            raise TransferError(f'{host} disk transfer failed (HTTP {http}, curl {code})')
        if http == 401:
            challenge = header(headers, 'www-authenticate') or challenge
            token = anonymous_token(challenge, repository)
            if failures == 1:
                continue
        time.sleep(retry_delay(headers, failures))


def copy_layout(reference, release, work, oras):
    layout = work / 'layout'
    blob_dir = layout / 'blobs' / 'sha256'
    blob_dir.mkdir(parents=True, exist_ok=True)
    disk, size = release['disk_digest'], release['disk_size']
    blob = blob_dir / disk.removeprefix('sha256:')
    mirror = 'ghcr.1ms.run/' + reference.split('/', 1)[1]
    with tempfile.NamedTemporaryFile(dir=work) as auth:
        auth.write(b'{"auths":{}}')
        auth.flush()
        for source in (reference, mirror):
            try:
                manifest, challenge, token = fetch_manifest(source)
                if manifest.get('artifactType') != 'application/vnd.shelter.artifact.v1' or not any(
                    layer['digest'] == disk and layer['size'] == size for layer in manifest.get('layers', [])
                ):
                    raise ValueError('OCI artifact does not contain the disk described by the release')
                download_disk(source, work / 'disk.part', blob, disk, size, challenge, token)
                subprocess.run([
                    oras, 'cp', '--recursive', '--to-oci-layout', '--from-registry-config',
                    auth.name, source, str(layout) + ':release',
                ], stdout=sys.stderr, check=True)
                return
            except (subprocess.CalledProcessError, TransferError) as error:
                if source == mirror:
                    raise
                log(f'{error}; trying ghcr.1ms.run')


def verify_artifact(shelter, artifact, manifest_digest, verify_disk):
    command = [shelter, 'image', 'verify', artifact / 'artifact-manifest.json', '--format', 'json']
    if not verify_disk:
        command.extend(['--release-manifest', manifest_digest])
    result = json.loads(subprocess.check_output(command))
    if result['manifest_digest'] != manifest_digest:
        raise ValueError('local artifact does not match the release manifest')


def artifact_for(metadata, output, shelter, oras, verify_disk=False):
    reference = metadata['oci_reference']
    release = json.loads(subprocess.check_output([
        shelter, 'image', 'inspect-release', metadata['image_release'],
        '--metadata-only', '--release-digest', metadata['release_digest'],
    ]))
    work = output / reference.rsplit('@sha256:', 1)[1]
    work.mkdir(exist_ok=True)
    artifact = work / 'artifact'
    layout = work / 'layout'
    with (work / 'download.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if artifact.is_dir():
            verify_artifact(shelter, artifact, release['manifest_digest'], verify_disk)
        else:
            copy_layout(reference, release, work, oras)
            with tempfile.TemporaryDirectory(dir=work) as staging:
                bundle = Path(staging) / 'artifact'
                digest = reference.rsplit('@', 1)[1]
                subprocess.run([oras, 'pull', '--oci-layout', f'{layout}@{digest}', '--output', bundle],
                               check=True, stdout=sys.stderr)
                verify_artifact(shelter, bundle, release['manifest_digest'], verify_disk)
                bundle.replace(artifact)
        if layout.is_dir():
            shutil.rmtree(layout)
        return artifact


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output-dir', type=Path, required=True)
    parser.add_argument('--version', default=SOURCE['version'])
    parser.add_argument('--from-dir', type=Path, help='use a trusted local copy of the official release metadata')
    parser.add_argument('--region', default='cn-beijing', help='region in the printed publish command')
    parser.add_argument('--format', choices=('text', 'json'), default='text')
    parser.add_argument('--verify-disk', action='store_true',
                        help="also inspect disk structure and boot evidence using Shelter's Linux disk tools")
    args = parser.parse_args()
    if args.verify_disk and sys.platform != 'linux':
        parser.error('--verify-disk requires Linux disk tools')
    output = args.output_dir.resolve()
    resolver = [sys.executable, HERE / 'fetch-release-image.py', '--output-dir', output,
                '--version', args.version]
    if args.from_dir is not None:
        resolver.extend(['--from-dir', args.from_dir])
    try:
        metadata = json.loads(subprocess.check_output(resolver))
        artifact = artifact_for(metadata, output, os.environ.get('CA_SHELTER_BIN', 'shelter'), TOOLS / 'oras',
                                args.verify_disk)
    except (OSError, ValueError, TransferError, subprocess.SubprocessError) as error:
        parser.exit(1, f'{error}\n')
    if args.format == 'json':
        print(json.dumps(dict(metadata, artifact=str(artifact))))
    else:
        print(f'Downloaded Shelter artifact: {artifact}')
        print(shlex.join([
            'confidential-agent', 'image', 'publish', '--artifact', str(artifact),
            '--image-release', metadata['image_release'], '--release-digest', metadata['release_digest'],
            '--region', args.region,
            *(['--verify-disk'] if args.verify_disk else []),
        ]))


def interrupt(_signum, _frame):
    raise KeyboardInterrupt


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, interrupt)
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
