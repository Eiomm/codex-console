#!/usr/bin/python3
"""Offline app-server fixture. All files and settings come from the test process."""
import json
import os
import sys
import time

assert sys.argv[1:] == ['app-server']
with open(os.environ['TEST_STARTS'], 'a') as file:
    file.write('start\n')
with open(os.environ['TEST_STARTS']) as file:
    generation = len(file.readlines())
with open(os.environ['TEST_STARTS'] + '.pid', 'w') as file:
    file.write(str(os.getpid()))
mode = os.environ.get('TEST_MODE', 'normal')
initialized = False
handshake = False
queued = []
expected = {
    'model/list': {'limit': 50},
    'account/rateLimits/read': None,
    'account/usage/read': None,
    'config/read': {'includeLayers': True},
}

settings_config = {'model': 'fast-model', 'model_reasoning_effort': 'high', 'service_tier': 'fast'}
settings_version = 1

def settings_reply(request):
    global settings_version
    method = request['method']
    with open(os.environ['TEST_STARTS'] + '.requests', 'a') as file:
        file.write(method + '\n')
    if method in expected:
        assert request['params'] == expected[method]
    if method == 'config/read':
        time.sleep(0.03)  # Give a concurrent caller time to expose missing transaction locking.
        result = {'config': settings_config, 'origins': {'PRIVATE_ACCOUNT_DATA': True}, 'layers': [
            {'name': {'type': 'system'}, 'version': 'not-user', 'config': {'secret': 'PRIVATE_ACCOUNT_DATA'}},
            {'name': {'type': 'user'}, 'version': 'sha256:v' + str(settings_version), 'config': {'secret': 'PRIVATE_ACCOUNT_DATA'}}]}
    elif method == 'model/list':
        result = {'data': [
            {'model': 'fast-model', 'defaultReasoningEffort': 'high', 'supportedReasoningEfforts': [{'reasoningEffort': 'high'}, {'reasoningEffort': 'low'}], 'serviceTiers': [{'id': 'priority'}]},
            {'model': 'slow-model', 'defaultReasoningEffort': 'low', 'supportedReasoningEfforts': [{'reasoningEffort': 'low'}]}]}
    else:
        # This branch belongs exclusively to the update_settings fixture path.
        assert method == 'config/batchWrite'
        params = request['params']
        assert set(params) == {'edits', 'expectedVersion', 'reloadUserConfig'}
        assert params['reloadUserConfig'] is False
        for edit in params['edits']:
            assert set(edit) == {'keyPath', 'value', 'mergeStrategy'}
            assert edit['keyPath'] in ('model', 'model_reasoning_effort', 'service_tier')
            assert edit['mergeStrategy'] == 'replace'
        if mode == 'settings-error':
            print(json.dumps({'id': request['id'], 'error': {'code': -32600, 'data': {'config_write_error_code': 'other'}, 'message': 'PRIVATE_ACCOUNT_DATA'}}), flush=True)
            return
        if mode == 'settings-conflict' or params['expectedVersion'] != 'sha256:v' + str(settings_version):
            settings_config['model_reasoning_effort'] = 'low'
            settings_version += 1
            print(json.dumps({'id': request['id'], 'error': {'code': -32600, 'data': {'config_write_error_code': 'configVersionConflict'}, 'message': 'PRIVATE_ACCOUNT_DATA'}}), flush=True)
            return
        for edit in params['edits']:
            if edit['value'] is None:
                settings_config.pop(edit['keyPath'], None)
            else:
                settings_config[edit['keyPath']] = edit['value']
        settings_version += 1
        result = {'status': 'ok', 'version': 'sha256:v' + str(settings_version), 'filePath': 'PRIVATE_ACCOUNT_DATA', 'overriddenMetadata': None}
        if mode == 'settings-overridden':
            settings_config['model_reasoning_effort'] = 'high'
            result.update(status='okOverridden', overriddenMetadata={'message': 'PRIVATE_ACCOUNT_DATA', 'overridingLayer': {'name': {'type': 'system', 'private': 'PRIVATE_ACCOUNT_DATA'}, 'version': 'PRIVATE_ACCOUNT_DATA'}, 'effectiveValue': 'high'})
    print(json.dumps({'id': request['id'], 'result': result}), flush=True)

def reply(request):
    if mode == 'partial' and request['method'] == 'account/usage/read':
        print(json.dumps({'id': request['id'], 'error': {'message': 'PRIVATE_ACCOUNT_DATA'}}), flush=True)
    else:
        print(json.dumps({'id': request['id'], 'result': {'method': request['method']}}), flush=True)

for line in sys.stdin:
    request = json.loads(line)
    assert 'jsonrpc' not in request
    method = request['method']
    if method == 'initialize':
        assert not handshake
        assert request['params'] == {'clientInfo': {'name': 'codex-console', 'title': 'Codex 控制台', 'version': '0.1.0'}, 'capabilities': {'experimentalApi': True}}
        handshake = True
        time.sleep(0.04)
        print('not JSON', flush=True)
        print(json.dumps({'method': 'notice'}), flush=True)
        print(json.dumps({'id': request['id'], 'method': 'notification', 'result': 'ignored'}), flush=True)
        print(json.dumps({'id': 999999, 'result': 'unrelated'}), flush=True)
        print(json.dumps({'id': request['id'], 'result': {}}), flush=True)
    elif method == 'initialized':
        assert handshake and not initialized
        assert request == {'method': 'initialized'}
        initialized = True
    else:
        assert initialized
        if mode.startswith('settings-'):
            settings_reply(request)
            continue
        assert request['params'] == expected[method]
        if mode == 'hang':
            continue
        if mode == 'crash' and generation == 1:
            queued.append(request)
            if len(queued) == 5:
                sys.exit(0)
            continue
        if mode == 'reverse':
            queued.append(request)
            if len(queued) == 5:
                for item in reversed(queued):
                    reply(item)
                queued.clear()
        else:
            reply(request)
