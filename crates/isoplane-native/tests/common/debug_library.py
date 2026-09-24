"""Bind source symbols from a referenced managed project."""
from pathlib import Path
from debug_sources import same_source

EXPECTED_RESPONSE = b'42'


def configure(source, send, response):
    line = next(index + 1 for index, text in enumerate(Path(source).read_text().splitlines()) if 'return result;' in text)
    response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': [{'line': line}]}))


def exercise(source, send, response, until):
    event = until(lambda item: item.get('event') == 'stopped')['body']
    assert event['reason'] == 'breakpoint', 'the referenced library must bind its source breakpoint'
    frame = response(send('stackTrace', {'threadId': event['threadId']}))['body']['stackFrames'][0]
    assert same_source(frame['source']['path'], source) and 'Calculator.Work' in frame['name'], 'the debugger must stop in the referenced project source'
    scopes = response(send('scopes', {'frameId': frame['id']}))['body']['scopes']
    scope = next(scope['variablesReference'] for scope in scopes if scope['name'] == 'Locals')
    values = response(send('variables', {'variablesReference': scope}))['body']['variables']
    assert any(value['name'] == 'result' and value['value'] == '42' for value in values), 'the referenced library must expose its actual locals'
    response(send('continue', {'threadId': event['threadId']}))
