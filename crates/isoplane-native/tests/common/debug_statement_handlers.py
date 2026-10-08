"""Move within active handlers, preserving rethrow and normal/exceptional finally exits."""
from pathlib import Path

EXPECTED_RESPONSE = b'original:24:0'


def locations(source):
    lines = Path(source).read_text().splitlines()
    return {text: next(index + 1 for index, line in enumerate(lines) if text in line)
            for text in ('skipped += 100;', 'throw;', 'cleanup += 1;', 'skipped += 1000;', 'cleanup += 10;')}


def configure(source, send, response):
    lines = locations(source)
    response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': [
        {'line': lines['skipped += 100;']}, {'line': lines['cleanup += 1;']}]}))


def exercise(source, send, response, until):
    lines = locations(source)

    def stop(line):
        event = until(lambda item: item.get('event') == 'stopped')
        thread = event['body']['threadId']
        frame = response(send('stackTrace', {'threadId': thread}))['body']['stackFrames'][0]
        assert frame['line'] == lines[line], (event, frame)
        return thread, frame

    def targets(line):
        return response(send('gotoTargets', {'source': {'path': source}, 'line': lines[line]}))['body']['targets']

    def jump(thread, line):
        choices = targets(line)
        assert choices, f'active handler must offer jump targets: {line}'
        response(send('goto', {'threadId': thread, 'targetId': choices[0]['id']}))
        return stop(line)

    thread, frame = stop('skipped += 100;')
    assert not targets('cleanup += 1;'), 'a catch jump cannot enter its finally'
    assert response(send('evaluate', {'expression': 'original', 'frameId': frame['id']}))['body']['result'] == '"original"'
    thread, frame = jump(thread, 'throw;')
    response(send('continue', {'threadId': thread}))
    for invocation in range(2):
        thread, frame = stop('cleanup += 1;')
        if invocation == 1:
            response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': []}))
        assert not targets('throw;'), 'a finally jump cannot enter a catch'
        response(send('next', {'threadId': thread}))
        thread, frame = stop('skipped += 1000;')
        thread, frame = jump(thread, 'cleanup += 1;')
        response(send('next', {'threadId': thread}))
        thread, frame = stop('skipped += 1000;')
        thread, frame = jump(thread, 'cleanup += 10;')
        response(send('continue', {'threadId': thread}))
