#!/usr/bin/env python3.11
"""Real deployment concurrency, interruption, frozen inputs and stable routes."""
import json
import os
import shutil
from cloud_case import Case, ROOT, release_selection, run, write_json
from fixtures.allinone import generate


def add_platform_fixtures(work, selected, prefix):
    """Reuse native consumers for route churn after their real model probes."""
    # Two peers share a mesh port; the client has no business port.
    peers = [fixture for fixture in selected if fixture.name in {'openclaw', 'hermes'}]
    assert len(peers) == 2
    for fixture in peers:
        path = fixture / 'appspec.json'
        spec = json.loads(path.read_text())
        spec['service'].update(ports=[8642], connect=[])
        write_json(path, spec)
        if fixture.name == 'openclaw':
            runtime = fixture / 'runtime.json'
            configuration = json.loads(runtime.read_text())
            configuration.update(mode='gateway', port=8642)
            write_json(runtime, configuration)
            native = fixture / 'openclaw.json'
            configuration = json.loads(native.read_text())
            configuration['gateway']['port'] = 8642
            write_json(native, configuration)
        probe = fixture / 'probe.json'
        configuration = json.loads(probe.read_text())
        configuration['connect_http_path'] = None
        write_json(probe, configuration)
    extra = work / 'extra-fixture'
    shutil.copytree(next(fixture for fixture in peers if fixture.name == 'hermes'), extra)
    spec = json.loads((extra / 'appspec.json').read_text())
    spec['service']['id'] = prefix + '-aaa-extra'
    write_json(extra / 'appspec.json', spec)
    return selected, extra


def prepare_fixtures(work, image, region):
    selected = generate(work / 'fixtures', image, 'challenge', region, 'platform',
                        harnesses=['codex', 'openclaw', 'hermes'])
    return add_platform_fixtures(work, selected, 'platform')


def main():
    with Case() as case:
        cidr = case.operator()
        producer = case.suite / 'publish-reuse-images'
        image = release_selection(producer / 'allinone', case.region)
        selected, extra = prepare_fixtures(case.work, image, case.region)
        case.state = case.work / 'consumer/state'
        for fixture in [*selected, extra]:
            service = json.loads((fixture / 'appspec.json').read_text())['service']['id']
            case.register(service)
        case.register('zzzz-platform-aaa-extra')
        command = ['python3.11', ROOT / 'tools/e2e/allinone-reuse.py', '--work-dir', case.work / 'consumer',
                   '--ca', case.ca, '--operator-cidr', cidr, '--parallel', '2', '--concurrent-deploy',
                   '--interrupt-infrastructure', '--frozen-inputs', '--application-errors',
                   '--stable-endpoints-fixture', extra, '--publication-state', producer / 'state/services/allinone-builder/state.json']
        for fixture in selected:
            command += ['--fixture', fixture]
        run(command, case.work / 'platform', timeout=14400)
        result = json.loads((case.work / 'consumer/result.json').read_text())
        for name in ['passed', 'concurrent_deploy', 'interrupt_infrastructure', 'frozen_inputs',
                     'stable_endpoints', 'application_errors', 'shared_image_preserved']:
            assert result[name] is True, name + ' coverage did not execute'
        case.checks.update({name: result[name] for name in ['concurrent_deploy', 'interrupt_infrastructure',
                                                          'frozen_inputs', 'stable_endpoints', 'application_errors']})


if __name__ == '__main__':
    main()
