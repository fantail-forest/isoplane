"""Jump across an await without executing it, then await twice by jumping backward."""
from pathlib import Path

EXPECTED_RESPONSE = b'13:2'


def locations(source):
    lines = Path(source).read_text().splitlines()
    return {text: next(index + 1 for index, line in enumerate(lines) if text in line)
            for text in ('value += 1;', 'value += 10;', 'return $')}


def configure(source, send, response):
    response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': [
        {'line': locations(source)['value += 1;']}]}))


def exercise(source, send, response, until):
    lines = locations(source)

    def stop(line):
        event = until(lambda item: item.get('event') == 'stopped')
        thread = event['body']['threadId']
        frame = response(send('stackTrace', {'threadId': thread}))['body']['stackFrames'][0]
        assert frame['line'] == lines[line], frame
        return thread, frame

    def jump(thread, line):
        targets = response(send('gotoTargets', {'source': {'path': source}, 'line': lines[line]}))['body']['targets']
        assert targets, f'active async statement must offer jump targets: {line}'
        response(send('goto', {'threadId': thread, 'targetId': targets[0]['id']}))
        return stop(line)

    def value(frame, name):
        return response(send('evaluate', {'expression': name, 'frameId': frame['id']}))['body']['result']

    thread, frame = stop('value += 1;')
    thread, frame = jump(thread, 'value += 10;')
    assert value(frame, 'completed') == '0', 'a forward jump must skip the await and its following statements'
    assert value(frame, 'value') == '11'
    thread, frame = jump(thread, 'value += 1;')
    response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': [
        {'line': lines['value += 10;']}]}))
    response(send('continue', {'threadId': thread}))
    thread, frame = stop('value += 10;')
    assert value(frame, 'completed') == '1'
    assert value(frame, 'value') == '12'
    thread, frame = jump(thread, 'value += 1;')
    response(send('continue', {'threadId': thread}))
    thread, frame = stop('value += 10;')
    assert value(frame, 'completed') == '2', 'jumping backward across an await must allow it to suspend and resume again'
    assert value(frame, 'value') == '13'
    thread, frame = jump(thread, 'return $')
    response(send('continue', {'threadId': thread}))
