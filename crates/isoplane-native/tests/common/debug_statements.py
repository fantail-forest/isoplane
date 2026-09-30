"""Move the next statement through DAP and observe the resulting guest response."""
from pathlib import Path

EXPECTED_RESPONSE = b'11'


def locations(source):
    lines = Path(source).read_text().splitlines()
    return {text: next(index + 1 for index, line in enumerate(lines) if text in line)
            for text in ('answer += 10;', 'answer *= 2;', 'return answer;', 'private static string Handle()', 'int saved = answer;')}


def configure(source, send, response):
    response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': [
        {'line': locations(source)['answer += 10;']},
        {'line': locations(source)['int saved = answer;']}]}))


def exercise(source, send, response, until):
    lines = locations(source)

    def stop():
        event = until(lambda item: item.get('event') == 'stopped')
        thread = event['body']['threadId']
        frame = response(send('stackTrace', {'threadId': thread}))['body']['stackFrames'][0]
        return thread, frame

    def target(line):
        return response(send('gotoTargets', {'source': {'path': source}, 'line': lines[line]}))['body']['targets']

    def rejected(command, arguments):
        identifier = send(command, arguments)
        result = until(lambda item: item.get('type') == 'response' and item.get('request_seq') == identifier)
        assert not result.get('success'), 'invalid or expired execution targets must be rejected'

    thread, frame = stop()
    assert frame['line'] == lines['answer += 10;']
    assert not target('private static string Handle()'), 'execution cannot jump into a different method'
    forward = target('return answer;')
    assert forward, 'a statement with an empty evaluation stack must be a jump target'
    rejected('goto', {'threadId': 2147483647, 'targetId': forward[0]['id']})
    response(send('goto', {'threadId': thread, 'targetId': forward[0]['id']}))
    thread, frame = stop()
    assert frame['line'] == lines['return answer;'], 'the displayed program counter must move immediately'
    assert response(send('evaluate', {'expression': 'answer', 'frameId': frame['id']}))['body']['result'] == '1', 'jumping must not execute skipped statements'
    rejected('goto', {'threadId': thread, 'targetId': forward[0]['id']})
    backward = target('answer += 10;')
    assert backward, 'a previous statement in the same method must be selectable'
    response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': [
        {'line': lines['int saved = answer;']}]}))
    response(send('goto', {'threadId': thread, 'targetId': backward[0]['id']}))
    thread, frame = stop()
    assert frame['line'] == lines['answer += 10;']
    response(send('next', {'threadId': thread}))
    thread, frame = stop()
    assert frame['line'] == lines['answer *= 2;'], f'stepping must start from the selected statement: {frame}'
    destination = target('return answer;')[0]
    response(send('goto', {'threadId': thread, 'targetId': destination['id']}))
    thread, frame = stop()
    response(send('continue', {'threadId': thread}))
    thread, frame = stop()
    assert frame['line'] == lines['int saved = answer;']
    assert not target('int saved = answer;'), 'methods with finally regions must not permit moving execution'
    response(send('continue', {'threadId': thread}))
