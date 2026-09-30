"""Filtered local-watchpoint stops preserve stepping across calls and scope exit."""
from pathlib import Path

from debug_watchpoints import EXPECTED_RESPONSE


def configure(source, send, response):
    lines = Path(source).read_text().splitlines()
    line = next(i + 1 for i, text in enumerate(lines) if 'local-watch-start' in text)
    response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': [{'line': line, 'hitCondition': '1'}]}))


def exercise(source, send, response, until):
    def stop(reason, method):
        event = until(lambda item: item.get('event') == 'stopped')['body']
        assert event['reason'] == reason, event
        frame = response(send('stackTrace', {'threadId': event['threadId']}))['body']['stackFrames'][0]
        assert method in frame['name'], frame
        return event, frame

    event, frame = stop('breakpoint', 'LocalWork')
    scope = response(send('scopes', {'frameId': frame['id']}))['body']['scopes'][0]['variablesReference']
    target = response(send('dataBreakpointInfo', {'variablesReference': scope, 'name': 'value'}))['body']['dataId']
    assert target, 'the caller local must be watchable before stepping'
    response(send('setDataBreakpoints', {'breakpoints': [{'dataId': target, 'condition': 'false'}]}))
    for expected in ('10', '11', '12'):
        response(send('next', {'threadId': event['threadId']}))
        event, frame = stop('step', 'LocalWork')
        value = response(send('evaluate', {'frameId': frame['id'], 'expression': 'value'}))['body']['result']
        assert value == expected, (value, expected)
    installed = response(send('setDataBreakpoints', {'breakpoints': [{'dataId': target, 'hitCondition': '100'}]}))['body']['breakpoints'][0]
    assert installed['verified']
    response(send('stepOut', {'threadId': event['threadId']}))
    expired = until(lambda item: item.get('event') == 'breakpoint' and item['body']['breakpoint'].get('id') == installed['id'] and not item['body']['breakpoint']['verified'])
    assert 'scope' in expired['body']['breakpoint']['message']
    event, _ = stop('step', 'LocalWatchJourney')
    response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': []}))
    response(send('continue', {'threadId': event['threadId']}))
