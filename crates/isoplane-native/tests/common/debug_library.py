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
    frames = response(send('stackTrace', {'threadId': event['threadId']}))['body']['stackFrames']
    names = [frame['name'] for frame in frames]
    assert not [name for name in names if name.startswith('Isoplane')], f'Isoplane runtime and SDK frames must not appear in the stack: {names}'
    handler = next((index for index, name in enumerate(names) if name.endswith('.Application.Handle')), None)
    assert handler is not None, f'the stack must retain the customer caller of a referenced library: {names}'
    assert any(name.startswith('System.') for name in names[1:handler]), f'the framework method the handler calls the library through must appear between them: {names}'
    assert any(name.startswith('Microsoft.AspNetCore.') for name in names[handler + 1:]), f'the ASP.NET Core frames that invoke the handler must appear below it: {names}'
    assert names[-1].startswith('Microsoft.AspNetCore.'), f'the stack must end at the outermost package frame, without the runtime frames that invoke it: {names}'
    frame = frames[0]
    assert same_source(frame['source']['path'], source) and 'Calculator.Work' in frame['name'], 'the debugger must stop in the referenced project source'
    scopes = response(send('scopes', {'frameId': frame['id']}))['body']['scopes']
    scope = next(scope['variablesReference'] for scope in scopes if scope['name'] == 'Locals')
    values = response(send('variables', {'variablesReference': scope}))['body']['variables']
    assert any(value['name'] == 'result' and value['value'] == '42' for value in values), 'the referenced library must expose its actual locals'
    response(send('continue', {'threadId': event['threadId']}))
