"""Qualify the uncaught filter against real caught and unhandled guest exceptions."""
from pathlib import Path
from debug_sources import stop_event

EXPECTED_STATUS = 500
EXPECTED_RESPONSE = b''
request_index = 0


def configure(source, send, response):
    line = next(index + 1 for index, text in enumerate(Path(source).read_text().splitlines())
                if 'if (!request.Query.ContainsKey("fatal"))' in text)
    response(send('setBreakpoints', {'source': {'path': source}, 'breakpoints': [{'line': line}]}))
    response(send('setExceptionBreakpoints', {'filters': ['uncaught']}))


def exercise(source, send, response, until):
    global request_index, EXPECTED_RESPONSE
    fatal = request_index >= 2
    filtered = request_index % 2 == 1
    request_index += 1
    EXPECTED_RESPONSE = b'guest_panic: guest returned trapped(Panic)' if fatal else b''
    stopped = until(stop_event)
    assert stopped['body']['reason'] == 'breakpoint', 'the uncaught filter must skip the caught exception'
    thread = stopped['body']['threadId']
    frames = response(send('stackTrace', {'threadId': thread}))['body']['stackFrames']
    value = response(send('evaluate', {'expression': 'caught', 'frameId': frames[0]['id'], 'context': 'watch'}))['body']
    assert value['result'] == '1', 'the caught exception must have reached its handler'
    attached = response(send('evaluate', {'expression': 'System.Diagnostics.Debugger.IsAttached', 'frameId': frames[0]['id'], 'context': 'watch'}))['body']
    assert attached['result'] == 'true', 'the runtime must expose its actual debugger attachment state'
    response(send('setExceptionBreakpoints', {'filters': [] if filtered else ['uncaught']}))
    response(send('continue', {'threadId': thread}))
    if filtered:
        return
    stopped = until(stop_event)
    assert stopped['body']['reason'] == 'exception', 'an unhandled exception must stop execution'
    thread = stopped['body']['threadId']
    exception = response(send('exceptionInfo', {'threadId': thread}))['body']
    assert exception['exceptionId'] == 'System.InvalidOperationException', exception
    assert ('debug-unhandled' if fatal else 'debug-request-failed') in exception['description'], exception
    assert exception['breakMode'] == 'unhandled', exception
    assert ('FailWithoutTask' if fatal else 'Handle') in exception['details']['stackTrace'], exception
    response(send('continue', {'threadId': thread}))
