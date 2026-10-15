"""Bind source symbols from a referenced managed project."""
from pathlib import Path
from debug_sources import same_source, stop_event

EXPECTED_RESPONSE = b'42'


def configure(source, send, response):
    line = next(index + 1 for index, text in enumerate(Path(source).read_text().splitlines()) if 'return result;' in text)
    response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': [{'line': line}]}))


def exercise(source, send, response, until):
    event = until(stop_event)['body']
    assert event['reason'] == 'breakpoint', 'the referenced library must bind its source breakpoint'
    frames = response(send('stackTrace', {'threadId': event['threadId']}))['body']['stackFrames']
    names = [frame['name'] for frame in frames]
    machinery = ('Isoplane.MonoRuntime.', 'Isoplane.Abi.', 'Isoplane.Runtime.', 'IsoplaneGeneratedExports')
    assert not [name for name in names if name.startswith(machinery)], f'runtime machinery frames must not appear in the stack: {names}'
    handler = next((index for index, name in enumerate(names) if name.endswith('.Application.Library')), None)
    assert handler is not None, f'the stack must retain the customer caller of a referenced library: {names}'
    assert any(name.startswith('System.') for name in names[1:handler]), f'the framework method the handler calls the library through must appear between them: {names}'
    assert len(names) - 1 > handler and names[-1].startswith('Isoplane.'), f'the stack must end at the SDK frame that dispatches to the handler, without the runtime frames that invoke it: {names}'
    frame = frames[0]
    assert same_source(frame['source']['path'], source) and 'Calculator.Work' in frame['name'], 'the debugger must stop in the referenced project source'
    scopes = response(send('scopes', {'frameId': frame['id']}))['body']['scopes']
    scope = next(scope['variablesReference'] for scope in scopes if scope['name'] == 'Locals')
    values = response(send('variables', {'variablesReference': scope}))['body']['variables']
    assert any(value['name'] == 'result' and value['value'] == '42' for value in values), 'the referenced library must expose its actual locals'
    response(send('continue', {'threadId': event['threadId']}))
