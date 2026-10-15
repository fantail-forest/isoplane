"""Prove managed data breakpoints against interpreter writes and garbage collection."""
from pathlib import Path
import debug_local_watchpoints
from debug_sources import stop_event

EXPECTED_RESPONSE = b'7:4:0:0:old:0:31'


def configure(source, send, response):
    line = next(index + 1 for index, text in enumerate(Path(source).read_text().splitlines()) if 'int checkpoint = 0;' in text)
    response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': [{'line': line}]}))


def exercise(source, send, response, until):
    def stop(reason):
        event = until(stop_event)['body']
        assert event['reason'] == reason, 'managed writes must report a data breakpoint stop'
        thread = event['threadId']
        frame = response(send('stackTrace', {'threadId': thread}))['body']['stackFrames'][0]
        assert 'Handle' in frame['name'], 'a data breakpoint must stop in the customer method performing the write'
        return event, frame

    def rejected(command, arguments):
        identifier = send(command, arguments)
        result = until(lambda item: item.get('type') == 'response' and item.get('request_seq') == identifier)
        assert not result.get('success'), 'invalid debugger requests must fail without changing active watchpoints'

    event, frame = stop('breakpoint')
    thread = event['threadId']
    scope = response(send('scopes', {'frameId': frame['id']}))['body']['scopes'][0]['variablesReference']
    values = response(send('variables', {'variablesReference': scope}))['body']['variables']
    sample = next(value for value in values if value['name'] == 'sample')
    numbers = next(value for value in values if value['name'] == 'numbers')
    requests = [
        {'variablesReference': sample['variablesReference'], 'name': 'Value'},
        {'variablesReference': numbers['variablesReference'], 'name': '[1]'},
        {'frameId': frame['id'], 'name': 'grid[1, 0]'},
        {'frameId': frame['id'], 'name': 'StaticValue'},
        {'variablesReference': sample['variablesReference'], 'name': 'Text'},
        {'frameId': frame['id'], 'name': 'sample.Box'},
        {'frameId': frame['id'], 'name': 'references[0]'},
        {'frameId': frame['id'], 'name': 'sample.Pair'},
        {'frameId': frame['id'], 'name': 'pairs[0]'},
    ]
    targets = [response(send('dataBreakpointInfo', arguments))['body'] for arguments in requests]
    for target in targets:
        assert target['dataId'], 'managed fields and array elements must resolve to watchable storage'
        assert target['accessTypes'] == ['write'], 'the editor must offer value-change watchpoints'
        assert target['canPersist'] is False, 'managed object locations must not be reused across guest sessions'
    for expression in ('numbers[-1]', 'numbers[3]', 'numbers[0.5]', 'grid[0]', 'sample.Value[0]', 'sample.Missing', 'sample.', 'numbers[long.MinValue]', 'numbers[ulong.MaxValue]'):
        unavailable = response(send('dataBreakpointInfo', {'frameId': frame['id'], 'name': expression}))['body']
        assert unavailable['dataId'] is None, f'{expression} must not be advertised as watchable managed storage'
    unavailable = response(send('dataBreakpointInfo', {'frameId': frame['id'], 'name': '0x100', 'asAddress': True}))['body']
    assert unavailable['dataId'] is None, 'raw memory watchpoints must remain outside the supported scope'
    integral = response(send('dataBreakpointInfo', {'frameId': frame['id'], 'name': 'numbers[1L]'}))['body']
    assert integral['dataId'], 'integral C# array indices must resolve to the same managed element storage'
    stale = response(send('dataBreakpointInfo', {'frameId': frame['id'], 'name': 'numbers[0]'}))['body']['dataId']
    configured = response(send('setDataBreakpoints', {'breakpoints': [{'dataId': target['dataId']} for target in targets]}))['body']['breakpoints']
    assert all(item['verified'] for item in configured), 'the runtime must confirm each managed watchpoint'
    response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': []}))
    old_frame = frame['id']
    response(send('continue', {'threadId': thread}))
    changes = [
        (0, 'sample.Value', '11'), (0, 'sample.Value', '7'),
        (1, 'numbers[1]', '12'), (1, 'numbers[1]', '4'),
        (2, 'grid[1, 0]', '9'), (2, 'grid[1, 0]', '0'),
        (3, 'StaticValue', '3'), (3, 'StaticValue', '0'),
        (4, 'sample.Text', '"new"'), (4, 'sample.Text', '"old"'),
        (5, '(int)sample.Box', '42'), (5, '(int)sample.Box', '42'),
        (6, '(int)references[0]', '42'), (6, '(int)references[0]', '42'),
        (7, '(int)sample.Pair.Item1', '42'), (7, '(int)sample.Pair.Item1', '42'),
        (8, '(int)pairs[0].Item1', '42'), (8, '(int)pairs[0].Item1', '42'),
    ]
    for index, (target, expression, value) in enumerate(changes):
        event, frame = stop('data breakpoint')
        assert event['hitBreakpointIds'] == [configured[target]['id']], 'the stop must identify the changed storage location'
        observed = response(send('evaluate', {'frameId': frame['id'], 'expression': expression}))['body']['result']
        assert observed == value, 'watchpoints must catch both changes on a single source line, including after garbage collection'
        if target == 7:
            named = response(send('evaluate', {'frameId': frame['id'], 'expression': '(int)sample.Pair.Item'}))['body']['result']
            assert named == value, 'tuple elements must remain inspectable by both their source name and Item1 after collection'
        if index == 0:
            rejected('setDataBreakpoints', {'breakpoints': [{'dataId': stale}]})
            rejected('evaluate', {'frameId': old_frame, 'expression': 'sample.Value'})
        if index != len(changes) - 1:
            response(send('continue', {'threadId': thread}))
    for condition, expected in [('42', '8'), ('sample.Missing == 0', '9')]:
        requested = [{'dataId': target['dataId']} for target in targets]
        requested[0]['condition'] = condition
        response(send('setDataBreakpoints', {'breakpoints': requested}))
        response(send('continue', {'threadId': thread}))
        event, frame = stop('data breakpoint')
        assert 'condition failed' in event['description'], 'invalid conditions must stop with an actionable explanation'
        observed = response(send('evaluate', {'frameId': frame['id'], 'expression': 'sample.Value'}))['body']['result']
        assert observed == expected, 'a bad condition must leave the paused guest inspectable'
    response(send('setDataBreakpoints', {'breakpoints': [
        {'dataId': targets[0]['dataId'], 'condition': 'sample.Value == 11', 'hitCondition': '>= 2'},
        {'dataId': targets[1]['dataId'], 'hitCondition': '2'},
    ]}))
    response(send('continue', {'threadId': thread}))
    for target, expression, expected in [(0, 'sample.Value', '11'), (1, 'numbers[1]', '12')]:
        event, frame = stop('data breakpoint')
        assert event['hitBreakpointIds'] == [configured[target]['id']], 'conditional watchpoints must retain their editor identity'
        actual = response(send('evaluate', {'frameId': frame['id'], 'expression': expression}))['body']['result']
        assert actual == expected, 'conditions and hit counts must skip nonmatching managed changes'
        if target == 1:
            response(send('setDataBreakpoints', {'breakpoints': []}))
            debug_local_watchpoints.configure(source, send, response)
        response(send('continue', {'threadId': thread}))
    debug_local_watchpoints.exercise(source, send, response, until)
