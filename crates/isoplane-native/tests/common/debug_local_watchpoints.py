"""Watch actual interpreter locals through scope exit, calls and exception unwinding."""
from pathlib import Path


def configure(source, send, response):
    markers = ('local-watch-start', 'local-watch-scope', 'local-watch-throw', 'local-watch-finished')
    lines = Path(source).read_text().splitlines()
    points = [{'line': next(i + 1 for i, line in enumerate(lines) if marker in line), 'hitCondition': '1'} for marker in markers]
    response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': points}))


def exercise(source, send, response, until):
    def stop(reason, method):
        event = until(lambda item: item.get('event') == 'stopped')['body']
        assert event['reason'] == reason, event
        frames = response(send('stackTrace', {'threadId': event['threadId']}))['body']['stackFrames']
        frame = next(frame for frame in frames if method in frame['name'])
        return event, frame

    def select(frame, name):
        scope = response(send('scopes', {'frameId': frame['id']}))['body']['scopes'][0]['variablesReference']
        target = response(send('dataBreakpointInfo', {'variablesReference': scope, 'name': name}))['body']
        assert target['dataId'], f'{name} must resolve to a live local: {target}'
        assert target['canPersist'] is False
        return target['dataId']

    def install(targets):
        items = response(send('setDataBreakpoints', {'breakpoints': [{'dataId': target} for target in targets]}))['body']['breakpoints']
        assert all(item['verified'] for item in items), items
        return [item['id'] for item in items]

    def resume(event):
        response(send('continue', {'threadId': event['threadId']}))

    def value(frame, name, expected):
        actual = response(send('evaluate', {'frameId': frame['id'], 'expression': name}))['body']['result']
        assert actual == expected, (name, actual, expected)

    def expired(identifier):
        event = until(lambda item: item.get('event') == 'breakpoint' and item['body']['breakpoint'].get('id') == identifier and not item['body']['breakpoint']['verified'])
        assert 'scope' in event['body']['breakpoint']['message'], event

    event, frame = stop('breakpoint', 'LocalWork')
    local = select(frame, 'value')
    local_id, = install([local])
    resume(event)
    for expected in ('11', '12', '13', '14'):
        event, frame = stop('data breakpoint', 'LocalWork')
        assert event['hitBreakpointIds'] == [local_id], event
        value(frame, 'value', expected)
        if expected == '12':
            assert install([local]) == [local_id], 'reconfiguring from a recursive callee must retain the caller storage'
        resume(event)

    event, frame = stop('breakpoint', 'LocalWork')
    scoped = select(frame, 'scoped')
    _, scoped_id = install([local, scoped])
    resume(event)
    event, frame = stop('data breakpoint', 'LocalWork')
    assert event['hitBreakpointIds'] == [scoped_id], event
    value(frame, 'scoped', '21')
    resume(event)
    expired(scoped_id)
    event, frame = stop('data breakpoint', 'LocalWork')
    assert event['hitBreakpointIds'] == [local_id], event
    value(frame, 'value', '15')
    rejected = send('setDataBreakpoints', {'breakpoints': [{'dataId': scoped}]})
    result = until(lambda item: item.get('type') == 'response' and item.get('request_seq') == rejected)
    assert not result.get('success'), 'an expired local must not bind to reused storage'
    response(send('stepOut', {'threadId': event['threadId']}))
    expired(local_id)
    event, frame = stop('step', 'LocalWatchJourney')
    resume(event)

    event, frame = stop('breakpoint', 'ThrowWork')
    doomed = select(frame, 'doomed')
    doomed_id, = install([doomed])
    resume(event)
    event, frame = stop('data breakpoint', 'ThrowWork')
    assert event['hitBreakpointIds'] == [doomed_id], event
    value(frame, 'doomed', '31')
    resume(event)
    expired(doomed_id)
    event, frame = stop('breakpoint', 'LocalWatchJourney')
    value(frame, 'result', '31')
    response(send('setDataBreakpoints', {'breakpoints': []}))
    response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': []}))
    resume(event)
