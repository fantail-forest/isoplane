"""Exercise managed debugger features through the installed CLI and a real guest."""
import re
from pathlib import Path
from debug_sources import stop_event

EXPECTED_RESPONSE = b'102:8:9:206'


def locations(source):
    lines = Path(source).read_text().splitlines()
    return {text: next(index + 1 for index, line in enumerate(lines) if text in line)
            for text in ('answer += 1;', 'int logged = answer;', 'int stepped = Compute(inspected);', 'int doubled = input * 2;', 'iteration += 1;')}


def configure(source, send, response):
    lines = locations(source)
    response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': [
        {'line': lines['answer += 1;'], 'condition': 'index == 1', 'hitCondition': '>= 2'},
        {'line': lines['int logged = answer;'], 'logMessage': 'iteration {index}: {answer}'},
        {'line': lines['int stepped = Compute(inspected);']},
        {'line': lines['iteration += 1;'], 'logMessage': 'pause-ready', 'hitCondition': '1'},
    ]}))
    namespace = re.search(r'namespace\s+([\w.]+);', Path(source).read_text()).group(1)
    response(send('setFunctionBreakpoints', {'breakpoints': [{'name': namespace + '.Application.ThrowForDebugger'}]}))
    response(send('setExceptionBreakpoints', {'filters': []}))


def exercise(source, send, response, until):
    lines = locations(source)

    def stop(reason=None):
        event = until(stop_event)
        if reason is not None:
            assert event['body']['reason'] == reason, 'the adapter must identify why execution paused'
        thread = event['body']['threadId']
        frames = response(send('stackTrace', {'threadId': thread}))['body']['stackFrames']
        assert frames, 'each pause must provide the real guest stack'
        return thread, frames[0]

    def evaluate(expression, frame):
        return response(send('evaluate', {'expression': expression, 'frameId': frame['id'], 'context': 'watch'}))['body']

    def locals_scope(frame):
        scopes = response(send('scopes', {'frameId': frame['id']}))['body']['scopes']
        return next(scope['variablesReference'] for scope in scopes if scope['name'] == 'Locals')

    thread, frame = stop()
    assert frame['line'] == lines['answer += 1;'], 'a conditional breakpoint must stop at its requested source line'
    assert evaluate('index', frame)['result'] == '1', 'the condition and hit count must skip the first loop iteration'
    assert evaluate('answer + numbers[1] + sample.Value', frame)['result'] == '52', 'watches must evaluate arithmetic, array access and object fields'
    hovered = response(send('evaluate', {'expression': 'sample.Value', 'frameId': frame['id'], 'context': 'hover'}))['body']
    assert hovered['result'] == '7' and hovered['type'] == 'int', 'hover must return the field value and managed type from the selected frame'
    scope = locals_scope(frame)
    values = response(send('variables', {'variablesReference': scope}))['body']['variables']
    numbers = next(value for value in values if value['name'] == 'numbers')
    children = response(send('variables', {'variablesReference': numbers['variablesReference']}))['body']['variables']
    assert any(value['value'] == '4' for value in children), 'array children must expose their actual values'
    for command, arguments in [('setVariable', {'variablesReference': scope, 'name': 'answer'}),
                               ('setExpression', {'frameId': frame['id'], 'expression': 'numbers[1]'})]:
        identifier = send(command, {**arguments, 'value': '"invalid integer"'})
        rejected = until(lambda item: item.get('type') == 'response' and item.get('request_seq') == identifier)
        assert not rejected.get('success'), 'an invalid assignment must report an error rather than return an unchanged value as success'
    assert evaluate('answer', frame)['result'] == '41', 'a rejected local edit must preserve its value'
    assert evaluate('numbers[1]', frame)['result'] == '4', 'a rejected array edit must preserve its value'
    changed = response(send('setVariable', {'variablesReference': scope, 'name': 'answer', 'value': '100'}))['body']
    assert changed['value'] == '100', 'editing a local must return its changed value'
    for expression, value in [('numbers[1]', '8'), ('sample.Value', '9')]:
        changed = response(send('setExpression', {'expression': expression, 'frameId': frame['id'], 'value': value}))['body']
        assert changed['value'] == value, 'editing an expression must update the selected guest value'
        assert evaluate(expression, frame)['result'] == value, 'watches must observe the edit immediately'
    response(send('continue', {'threadId': thread}))

    thread, frame = stop()
    assert frame['line'] == lines['int stepped = Compute(inspected);'], 'a false condition must not stop the final loop iteration'
    assert evaluate('answer', frame)['result'] == '102', 'guest execution must use the edited local'
    for message in ('iteration 0: 41', 'iteration 1: 101', 'iteration 2: 102'):
        until(lambda item: item.get('event') == 'output' and message in item['body'].get('output', ''))
    response(send('stepIn', {'threadId': thread}))
    thread, frame = stop()
    assert 'Compute' in frame['name'], 'step into must enter the called customer method'
    assert evaluate('input', frame)['result'] == '102', 'the called method must expose its argument'
    scope = locals_scope(frame)
    response(send('setVariable', {'variablesReference': scope, 'name': 'input', 'value': '103'}))
    response(send('stepOut', {'threadId': thread}))
    thread, frame = stop()
    assert 'Compute' not in frame['name'], 'step out must return to the caller'
    response(send('continue', {'threadId': thread}))

    until(lambda item: item.get('event') == 'output' and 'pause-ready' in item['body'].get('output', ''))
    invalid = send('pause', {'threadId': 2147483647})
    rejected = until(lambda item: item.get('type') == 'response' and item.get('request_seq') == invalid)
    assert not rejected.get('success'), 'pause must reject a thread that does not belong to the guest'
    response(send('pause', {'threadId': thread}))
    thread, frame = stop('pause')
    assert 'PauseForEditor' in frame['name'], 'pause must interrupt running customer code'
    response(send('setExpression', {'expression': 'ReleasePause', 'frameId': frame['id'], 'value': 'true'}))
    response(send('continue', {'threadId': thread}))

    thread, frame = stop()
    assert 'ThrowForDebugger' in frame['name'], 'a function breakpoint must bind and stop in the requested method'
    response(send('setExceptionBreakpoints', {'filters': ['all']}))
    response(send('continue', {'threadId': thread}))
    thread, frame = stop()
    exception = response(send('exceptionInfo', {'threadId': thread}))['body']
    assert exception['exceptionId'] == 'System.InvalidOperationException', 'exception stops must report the thrown type'
    assert 'debug-exception' in exception['description'], 'exception stops must preserve the customer exception message'
    response(send('setExceptionBreakpoints', {'filters': []}))
    response(send('continue', {'threadId': thread}))
